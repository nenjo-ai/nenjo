//! Chat command handlers.

use anyhow::{Context, Result};
use nenjo::commands::{LoadedCommand, find_command_manifest, find_invoked_command_manifest};
use nenjo::hooks::{ActiveHookScope, ResolvedHook};
use nenjo::manifest::CommandManifest;
use nenjo_sessions::{
    SessionStatus, SessionTranscriptAppend, SessionTranscriptEventPayload, SessionTransition,
    TranscriptState,
};
use std::path::{Component, Path, PathBuf};
use tokio::time::{Duration, Instant};
use tracing::{debug, info, trace, warn};
use uuid::Uuid;

use nenjo::Slug;
use nenjo_events::{DomainActivation, Response, StreamEvent};

use nenjo_harness::events::HarnessEvent;
use nenjo_harness::registry::ExecutionKind;
use nenjo_harness::request::ChatRequest;
use nenjo_harness::{Harness, HarnessError, ProviderRuntime};

use crate::event_bridge::{agent_name, summarize_stream_event, turn_event_to_stream_events};
use crate::handlers::ResponseSender;
use crate::handlers::notification::platform_notification_emitter;
use crate::resource_resolver::PlatformResourceResolver;
use crate::tools::{register_platform_notification_emitter, with_platform_notification_emitter};

const ASSISTANT_DELTA_FLUSH_CHARS: usize = 180;
const ASSISTANT_DELTA_FLUSH_AFTER: Duration = Duration::from_millis(75);

#[derive(Debug)]
struct PendingAssistantDelta {
    session_id: Uuid,
    run_id: String,
    request_id: String,
    delta: String,
    flush_at: Instant,
}

#[derive(Debug, Default)]
struct AssistantDeltaBuffer {
    pending: Option<PendingAssistantDelta>,
}

impl AssistantDeltaBuffer {
    fn push(
        &mut self,
        session_id: Uuid,
        event: StreamEvent,
        now: Instant,
    ) -> Vec<(Uuid, StreamEvent)> {
        let StreamEvent::AssistantTextDelta {
            run_id,
            request_id,
            payload,
            encrypted_payload,
        } = event
        else {
            let mut events = self.flush();
            events.push((session_id, event));
            return events;
        };

        if encrypted_payload.is_some() {
            let mut events = self.flush();
            events.push((
                session_id,
                StreamEvent::AssistantTextDelta {
                    run_id,
                    request_id,
                    payload,
                    encrypted_payload,
                },
            ));
            return events;
        }

        let Some(delta) = payload
            .as_ref()
            .and_then(|value| value.get("delta"))
            .and_then(serde_json::Value::as_str)
        else {
            let mut events = self.flush();
            events.push((
                session_id,
                StreamEvent::AssistantTextDelta {
                    run_id,
                    request_id,
                    payload,
                    encrypted_payload,
                },
            ));
            return events;
        };

        if delta.is_empty() {
            return Vec::new();
        }

        let same_request = self.pending.as_ref().is_some_and(|pending| {
            pending.session_id == session_id
                && pending.run_id == run_id
                && pending.request_id == request_id
        });
        if !same_request {
            let events = self.flush();
            self.pending = Some(PendingAssistantDelta {
                session_id,
                run_id,
                request_id,
                delta: delta.to_string(),
                flush_at: now + ASSISTANT_DELTA_FLUSH_AFTER,
            });
            if self
                .pending
                .as_ref()
                .is_some_and(|pending| pending.delta.chars().count() >= ASSISTANT_DELTA_FLUSH_CHARS)
            {
                return events.into_iter().chain(self.flush()).collect();
            }
            return events;
        }

        if let Some(pending) = self.pending.as_mut() {
            pending.delta.push_str(delta);
        }

        if self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.delta.chars().count() >= ASSISTANT_DELTA_FLUSH_CHARS)
        {
            self.flush()
        } else {
            Vec::new()
        }
    }

    fn next_flush_at(&self) -> Option<Instant> {
        self.pending.as_ref().map(|pending| pending.flush_at)
    }

    fn flush(&mut self) -> Vec<(Uuid, StreamEvent)> {
        let Some(pending) = self.pending.take() else {
            return Vec::new();
        };
        vec![(
            pending.session_id,
            StreamEvent::AssistantTextDelta {
                run_id: pending.run_id,
                request_id: pending.request_id,
                payload: Some(serde_json::json!({
                    "delta": pending.delta,
                })),
                encrypted_payload: None,
            },
        )]
    }
}

#[derive(Clone)]
pub struct ChatCommandContext<S> {
    pub response_sink: S,
    pub worker_id: String,
    pub state_dir: PathBuf,
}

pub struct ChatCommandRequest<'a> {
    pub message_id: Option<&'a str>,
    pub content: &'a str,
    pub project: Option<&'a str>,
    pub agent: Option<&'a str>,
    pub target_type: Option<&'a str>,
    pub target: Option<&'a str>,
    pub session_id: Uuid,
    pub domain_session_id: Option<Uuid>,
    pub domain_activation: Option<DomainActivation>,
    pub template_override: Option<String>,
    pub hook_scopes: Vec<ActiveHookScope>,
}

pub struct ChatSlashCommandRequest<'a> {
    pub message_id: Option<&'a str>,
    pub command: &'a str,
    pub content: &'a str,
    pub project: Option<&'a str>,
    pub agent: Option<&'a str>,
    pub target_type: Option<&'a str>,
    pub target: Option<&'a str>,
    pub session_id: Uuid,
    pub domain_session_id: Option<Uuid>,
    pub domain_activation: Option<DomainActivation>,
}

/// Worker integration methods for chat platform commands.
///
/// These methods adapt platform chat events to the platform-agnostic harness
/// chat API, then bridge harness events back into platform responses. Active
/// execution handles are registered by session id so cancellation and session
/// deletion can interrupt in-flight chats.
#[async_trait::async_trait]
pub(crate) trait WorkerChatHarnessExt<S>
where
    S: ResponseSender,
{
    /// Execute one chat message, including optional domain activation.
    async fn handle_chat(
        &self,
        ctx: &ChatCommandContext<S>,
        request: ChatCommandRequest<'_>,
    ) -> Result<()>
    where
        S: Clone + 'static;

    /// Execute one installed slash command by expanding its command markdown.
    async fn handle_chat_command(
        &self,
        ctx: &ChatCommandContext<S>,
        request: ChatSlashCommandRequest<'_>,
    ) -> Result<()>
    where
        S: Clone + 'static;

    /// Cancel the active chat execution for a chat session.
    async fn handle_chat_cancel(
        &self,
        ctx: &ChatCommandContext<S>,
        agent: Option<&str>,
        session_id: Option<Uuid>,
    ) -> Result<()>;

    /// Delete a chat session and cancel any active execution for that session.
    async fn handle_session_delete(
        &self,
        ctx: &ChatCommandContext<S>,
        project: &str,
        agent: &str,
        session_id: Uuid,
    ) -> Result<()>;
}

#[async_trait::async_trait]
impl<P, SessionRt, S> WorkerChatHarnessExt<S> for Harness<P, SessionRt>
where
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
    S: ResponseSender,
{
    async fn handle_chat(
        &self,
        ctx: &ChatCommandContext<S>,
        request: ChatCommandRequest<'_>,
    ) -> Result<()>
    where
        S: Clone + 'static,
    {
        handle_chat_adapter(self, ctx, request).await
    }

    async fn handle_chat_command(
        &self,
        ctx: &ChatCommandContext<S>,
        request: ChatSlashCommandRequest<'_>,
    ) -> Result<()>
    where
        S: Clone + 'static,
    {
        handle_chat_command_adapter(self, ctx, request).await
    }

    async fn handle_chat_cancel(
        &self,
        ctx: &ChatCommandContext<S>,
        agent: Option<&str>,
        session_id: Option<Uuid>,
    ) -> Result<()> {
        handle_chat_cancel(self, ctx, agent, session_id).await
    }

    async fn handle_session_delete(
        &self,
        ctx: &ChatCommandContext<S>,
        project: &str,
        agent: &str,
        session_id: Uuid,
    ) -> Result<()> {
        handle_session_delete(self, ctx, project, agent, session_id).await
    }
}

async fn handle_chat_adapter<P, SessionRt, S>(
    harness: &Harness<P, SessionRt>,
    ctx: &ChatCommandContext<S>,
    request: ChatCommandRequest<'_>,
) -> Result<()>
where
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
    S: ResponseSender + Clone + 'static,
{
    let ChatCommandRequest {
        message_id,
        content,
        project,
        agent,
        target_type,
        target,
        session_id,
        domain_session_id,
        domain_activation,
        template_override,
        hook_scopes,
    } = request;
    let input_message_id = message_id
        .map(Uuid::parse_str)
        .transpose()
        .context("chat message id must be a UUID")?;

    let command_template = if template_override.is_none() {
        load_matching_command_template(harness, content).await?
    } else {
        None
    };
    let template_override = template_override.or_else(|| {
        command_template
            .as_ref()
            .map(|template| template.content.clone())
    });
    let effective_content = command_template
        .as_ref()
        .map(|template| template.user_content.as_str())
        .unwrap_or(content);

    if target_type == Some("council") {
        let content = template_override.as_deref().unwrap_or(content);
        return handle_council_chat(
            harness,
            ctx,
            CouncilChatAdapterRequest {
                input_message_id,
                content,
                project,
                council: target.context("No council target provided for chat")?,
                session_id,
                domain_session_id,
                domain_activation,
            },
        )
        .await;
    }

    let agent_slug = agent
        .or(target)
        .map(Slug::parse)
        .transpose()?
        .context("No agent provided for chat")?;
    let manifest = harness.provider().manifest_snapshot();
    let resolver = PlatformResourceResolver::new(&manifest);
    let agent_id = resolver.agent_id(&agent_slug)?;
    let mut chat = ChatRequest::new(agent_slug.clone(), effective_content.to_string())
        .with_session(session_id);
    if let Some(input_message_id) = input_message_id {
        chat = chat.with_input_message_id(input_message_id);
    }
    if let Some(template_override) = template_override {
        chat = chat.with_template_override(template_override);
    }
    chat = chat.with_hook_transcript_dir(
        ctx.state_dir
            .join("sessions")
            .join(session_id.to_string())
            .join("hooks"),
    );
    if let Some(project) = project {
        chat = chat.with_project(Slug::parse(project)?);
    }
    if let Some(domain_session_id) = domain_session_id {
        chat = chat.with_domain_session(domain_session_id);
    }
    if let Some(activation) = domain_activation {
        chat = chat.with_domain_activation(
            activation.domain_session_id,
            activation.domain_command.clone(),
        );
    }
    let mut hook_scopes = hook_scopes;
    if let Some(template) = command_template
        && !template.hooks.is_empty()
    {
        hook_scopes.push(ActiveHookScope::command(&template.command, template.hooks));
    }
    for scope in hook_scopes {
        chat = chat.with_hook_scope(scope);
    }

    let provider = harness.provider();
    let manifest = provider.manifest_snapshot();
    let aname = agent_name(&manifest, agent_id);
    if harness.try_enqueue_chat_message(&chat).await? {
        debug!(
            session = %session_id,
            agent = %aname,
            "Queued chat message into active turn"
        );
        return Ok(());
    }
    let notification_emitter = platform_notification_emitter(ctx.response_sink.clone(), session_id);
    let _notification_registration =
        register_platform_notification_emitter(notification_emitter.clone());
    let mut stream =
        with_platform_notification_emitter(notification_emitter, harness.chat_stream(chat)).await?;
    let run_id = Uuid::new_v4().to_string();
    ctx.response_sink.send(Response::AgentResponse {
        session_id: Some(session_id),
        payload: StreamEvent::RunStarted {
            run_id: run_id.clone(),
            session_id: session_id.to_string(),
            input_message_id,
            parent_run_id: None,
            agent_id: Some(agent_slug.to_string()),
            agent_name: Some(aname.clone()),
        },
    })?;

    let mut assistant_delta_buffer = AssistantDeltaBuffer::default();
    loop {
        let event = if let Some(flush_at) = assistant_delta_buffer.next_flush_at() {
            tokio::select! {
                event = stream.recv() => event,
                _ = tokio::time::sleep_until(flush_at) => {
                    send_chat_stream_events(
                        &ctx.response_sink,
                        assistant_delta_buffer.flush(),
                        &aname,
                    );
                    continue;
                }
            }
        } else {
            stream.recv().await
        };

        let Some(event) = event else {
            break;
        };

        match event {
            HarnessEvent::DomainEntered {
                session_id: domain_session_id,
                domain_name,
            } => {
                send_chat_stream_events(&ctx.response_sink, assistant_delta_buffer.flush(), &aname);
                send_chat_stream_events(
                    &ctx.response_sink,
                    vec![(
                        session_id,
                        StreamEvent::DomainEntered {
                            session_id: domain_session_id,
                            domain_name,
                        },
                    )],
                    &aname,
                );
            }
            HarnessEvent::Turn {
                session_id: event_session_id,
                event: ev,
                ..
            } => {
                for mut se in turn_event_to_stream_events(&ev, &aname, &run_id, event_session_id) {
                    bind_chat_response_context(&mut se, &run_id, input_message_id);
                    let buffered_events =
                        assistant_delta_buffer.push(event_session_id, se, Instant::now());
                    send_chat_stream_events(&ctx.response_sink, buffered_events, &aname);
                }
            }
            HarnessEvent::Routine { .. } => {
                send_chat_stream_events(&ctx.response_sink, assistant_delta_buffer.flush(), &aname);
            }
        }
    }
    send_chat_stream_events(&ctx.response_sink, assistant_delta_buffer.flush(), &aname);

    debug!(session = %session_id, agent = %aname, "Chat harness event stream closed");
    debug!(session = %session_id, agent = %aname, "Awaiting chat stream output");
    let output = match stream.output().await {
        Ok(output) => output,
        Err(HarnessError::Cancelled) => {
            debug!(session = %session_id, agent = %aname, "Chat stream output cancelled");
            return Ok(());
        }
        Err(error) => return Err(error.into()),
    };
    debug!(
        session = %session_id,
        agent = %aname,
        text_len = output.text.len(),
        "Chat stream output completed"
    );
    Ok(())
}

fn bind_chat_response_context(
    event: &mut StreamEvent,
    run_id: &str,
    input_message_id: Option<Uuid>,
) {
    match event {
        StreamEvent::Done {
            run_id: event_run_id,
            input_message_id: event_input_message_id,
            ..
        } => {
            *event_run_id = Some(run_id.to_string());
            *event_input_message_id = input_message_id;
        }
        StreamEvent::AssistantResponse { payload, .. } => {
            let Some(payload) = payload.as_mut().and_then(serde_json::Value::as_object_mut) else {
                return;
            };
            payload.insert(
                "message_id".to_string(),
                serde_json::Value::String(Uuid::new_v4().to_string()),
            );
            if let Some(input_message_id) = input_message_id {
                payload.insert(
                    "input_message_id".to_string(),
                    serde_json::Value::String(input_message_id.to_string()),
                );
            }
        }
        StreamEvent::RunStarted { .. }
        | StreamEvent::RunCompleted { .. }
        | StreamEvent::RunFailed { .. }
        | StreamEvent::RunCancelled { .. }
        | StreamEvent::ModelRequestStarted { .. }
        | StreamEvent::AssistantTextDelta { .. }
        | StreamEvent::ModelRequestCompleted { .. }
        | StreamEvent::ToolCallStarted { .. }
        | StreamEvent::ToolOutputDelta { .. }
        | StreamEvent::ToolCallCompleted { .. }
        | StreamEvent::HookStarted { .. }
        | StreamEvent::HookCompleted { .. }
        | StreamEvent::AsyncOperationEvent { .. }
        | StreamEvent::AsyncOperationTranscript { .. }
        | StreamEvent::Error { .. }
        | StreamEvent::DomainEntered { .. }
        | StreamEvent::DomainExited { .. }
        | StreamEvent::MessageCompacted { .. }
        | StreamEvent::Paused
        | StreamEvent::Resumed => {}
    }
}

fn send_chat_stream_events<S>(response_sink: &S, events: Vec<(Uuid, StreamEvent)>, agent_name: &str)
where
    S: ResponseSender,
{
    for (session_id, event) in events {
        if matches!(event, StreamEvent::AssistantTextDelta { .. }) {
            trace!(
                stream_event = %summarize_stream_event(&event),
                agent = %agent_name,
                "Chat handler produced assistant text delta"
            );
        } else {
            debug!(
                stream_event = %summarize_stream_event(&event),
                agent = %agent_name,
                "Chat handler produced stream event"
            );
        }

        if let Err(error) = response_sink.send(Response::AgentResponse {
            session_id: Some(session_id),
            payload: event,
        }) {
            warn!(
                error = %error,
                session = %session_id,
                agent = %agent_name,
                "Failed to enqueue chat response"
            );
        }
    }
}

async fn handle_chat_command_adapter<P, SessionRt, S>(
    harness: &Harness<P, SessionRt>,
    ctx: &ChatCommandContext<S>,
    request: ChatSlashCommandRequest<'_>,
) -> Result<()>
where
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
    S: ResponseSender + Clone + 'static,
{
    let manifest = harness.provider().manifest_snapshot();
    let command_manifest = find_command_manifest(&manifest.commands, request.command)
        .with_context(|| format!("installed command not found: {}", request.command))?;
    let resolved_hooks = harness
        .provider()
        .resolve_hooks_for_command(command_manifest);
    let hook_scopes = if resolved_hooks.is_empty() {
        Vec::new()
    } else {
        vec![ActiveHookScope::command(command_manifest, resolved_hooks)]
    };
    let content =
        load_command_chat_template(command_manifest, request.command, request.content).await?;
    let user_content = command_arguments(request.command, request.content).to_string();

    handle_chat_adapter(
        harness,
        ctx,
        ChatCommandRequest {
            message_id: request.message_id,
            content: &user_content,
            project: request.project,
            agent: request.agent,
            target_type: request.target_type,
            target: request.target,
            session_id: request.session_id,
            domain_session_id: request.domain_session_id,
            domain_activation: request.domain_activation,
            template_override: Some(content),
            hook_scopes,
        },
    )
    .await
}

struct CommandTemplateOverride {
    content: String,
    user_content: String,
    command: CommandManifest,
    hooks: Vec<ResolvedHook>,
}

async fn load_matching_command_template<P, SessionRt>(
    harness: &Harness<P, SessionRt>,
    content: &str,
) -> Result<Option<CommandTemplateOverride>>
where
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
{
    let provider = harness.provider();
    let manifest = provider.manifest_snapshot();
    let Some(command) = find_invoked_command_manifest(&manifest.commands, content) else {
        return Ok(None);
    };
    Ok(Some(CommandTemplateOverride {
        content: load_command_chat_template(command, &command.command, content).await?,
        user_content: command_arguments(&command.command, content).to_string(),
        command: command.clone(),
        hooks: provider.resolve_hooks_for_command(command),
    }))
}

async fn load_command_chat_template(
    command: &CommandManifest,
    requested_command: &str,
    user_content: &str,
) -> Result<String> {
    let loaded = load_command(command).await?;
    Ok(command_chat_template(
        command,
        requested_command,
        user_content,
        &loaded,
    ))
}

async fn load_command(command: &CommandManifest) -> Result<LoadedCommand> {
    if !command.content.trim().is_empty() {
        return Ok(LoadedCommand {
            markdown: command.content.clone(),
            source_file: command.entry_path.clone(),
            command_dir: command.path.clone(),
            plugin_root: command
                .plugin_root_path
                .clone()
                .unwrap_or_else(|| command.path.clone()),
        });
    }

    let entry_file = command_entry_file(command)?;
    let markdown = tokio::fs::read_to_string(&entry_file)
        .await
        .with_context(|| format!("Failed to read command file {}", entry_file.display()))?;
    let plugin_root = command
        .plugin_root_dir
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| command.root_dir.display().to_string());
    Ok(LoadedCommand {
        markdown,
        source_file: entry_file.display().to_string(),
        command_dir: command.root_dir.display().to_string(),
        plugin_root,
    })
}

fn command_chat_template(
    command: &CommandManifest,
    requested_command: &str,
    user_content: &str,
    loaded: &LoadedCommand,
) -> String {
    let markdown = if command.source_type == "package" {
        strip_markdown_frontmatter(&loaded.markdown).unwrap_or(loaded.markdown.as_str())
    } else {
        loaded.markdown.as_str()
    };
    markdown.replace(
        "$ARGUMENTS",
        command_arguments(requested_command, user_content),
    )
}

fn strip_markdown_frontmatter(markdown: &str) -> Option<&str> {
    let rest = markdown.strip_prefix("---")?;
    let (_frontmatter, body) = rest.split_once("\n---")?;
    Some(body.trim_start_matches(['\r', '\n']))
}

fn command_arguments<'a>(requested_command: &str, user_content: &'a str) -> &'a str {
    let trimmed = user_content.trim();
    let command = requested_command.trim();
    let Some(rest) = trimmed.strip_prefix(command) else {
        return trimmed;
    };
    match rest.chars().next() {
        None => "",
        Some(ch) if ch.is_whitespace() => rest.trim(),
        Some(_) => trimmed,
    }
}

fn command_entry_file(command: &CommandManifest) -> Result<PathBuf> {
    if command.root_dir.as_os_str().is_empty() {
        anyhow::bail!("installed command {} is missing root_dir", command.command);
    }
    let entry_path = relative_manifest_path(&command.entry_path, "command entry_path")?;
    Ok(command.root_dir.join(entry_path))
}

fn relative_manifest_path<'a>(raw_path: &'a str, label: &str) -> Result<&'a Path> {
    let path = Path::new(raw_path);
    if raw_path.trim().is_empty() {
        anyhow::bail!("{label} must not be empty");
    }
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        anyhow::bail!("{label} must be a relative path inside the command root");
    }
    Ok(path)
}

struct CouncilChatAdapterRequest<'a> {
    input_message_id: Option<Uuid>,
    content: &'a str,
    project: Option<&'a str>,
    council: &'a str,
    session_id: Uuid,
    domain_session_id: Option<Uuid>,
    domain_activation: Option<DomainActivation>,
}

async fn handle_council_chat<P, SessionRt, S>(
    harness: &Harness<P, SessionRt>,
    ctx: &ChatCommandContext<S>,
    request: CouncilChatAdapterRequest<'_>,
) -> Result<()>
where
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
    S: ResponseSender + Clone + 'static,
{
    if request.domain_session_id.is_some() || request.domain_activation.is_some() {
        anyhow::bail!("Council chat does not support domain sessions");
    }

    let council = Slug::parse(request.council)?;
    let project = request.project.map(Slug::parse).transpose()?;
    let (events_tx, _events_rx) = tokio::sync::mpsc::unbounded_channel();
    let result = nenjo::routines::council::execute_council_chat(
        harness.provider().as_ref(),
        council.clone(),
        project.clone(),
        request.content.to_string(),
        request.session_id,
        &events_tx,
    )
    .await?;

    let payload = serde_json::json!({
        "final_output": result.output,
        "data": result.data,
        "target_type": "council",
        "target": council.into_string(),
    });
    ctx.response_sink.send(Response::AgentResponse {
        session_id: Some(request.session_id),
        payload: StreamEvent::Done {
            run_id: None,
            input_message_id: request.input_message_id,
            payload: Some(payload),
            encrypted_payload: None,
            total_input_tokens: result.input_tokens,
            total_output_tokens: result.output_tokens,
            project: project.map(|slug| slug.into_string()),
            agent: None,
            session_id: Some(request.session_id),
        },
    })?;

    Ok(())
}

/// Cancel in-flight chat executions.
///
/// `ChatCancel` is broadcast so the worker that owns the active session can see
/// it. New commands carry `session_id`; older commands fall back to cancelling
/// every active chat on the receiving worker.
async fn handle_chat_cancel<P, SessionRt, S>(
    harness: &Harness<P, SessionRt>,
    ctx: &ChatCommandContext<S>,
    agent: Option<&str>,
    session_id: Option<Uuid>,
) -> Result<()>
where
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
    S: ResponseSender,
{
    let keys_to_cancel: Vec<Uuid> = match session_id {
        Some(session_id) => harness
            .executions()
            .get(&session_id)
            .filter(|entry| entry.kind == ExecutionKind::Chat)
            .map(|_| vec![session_id])
            .unwrap_or_default(),
        None => harness
            .executions()
            .iter()
            .filter(|entry| entry.value().kind == ExecutionKind::Chat)
            .map(|entry| *entry.key())
            .collect(),
    };

    let mut cancelled = 0;
    for key in keys_to_cancel {
        if let Some((_, exec)) = harness.executions().remove(&key) {
            exec.cancel.cancel();
            let _ = harness
                .sessions()
                .append_transcript(SessionTranscriptAppend {
                    session_id: key,
                    turn_id: None,
                    payload: SessionTranscriptEventPayload::TurnInterrupted {
                        reason: "cancelled by user".to_string(),
                    },
                    transcript_state: TranscriptState::Clean,
                })
                .await;
            let _ = harness
                .sessions()
                .transition(SessionTransition {
                    session_id: key,
                    worker_id: ctx.worker_id.clone(),
                    phase: None,
                    status: SessionStatus::Cancelled,
                })
                .await;
            cancelled += 1;
        }
    }

    if cancelled > 0 {
        info!(agent = ?agent, ?session_id, cancelled, "Cancelled chat executions");
    }
    Ok(())
}

/// Delete a chat session's local history.
async fn handle_session_delete<P, SessionRt, S>(
    harness: &Harness<P, SessionRt>,
    _ctx: &ChatCommandContext<S>,
    _project: &str,
    _agent: &str,
    session_id: Uuid,
) -> Result<()>
where
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
    S: ResponseSender,
{
    let _ = harness.sessions().delete(session_id).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use nenjo::manifest::{
        AgentManifest, CommandManifest, HookCommandManifest, HookManifest, Manifest,
        McpServerManifest, ModelManifest, ProjectManifest, PromptConfig, SkillManifest,
        model_manifest_slug,
    };
    use nenjo::{
        AgentConfig, ModelProvider, ModelProviderFactory, Provider, Slug, Tool, ToolFactory,
    };
    use nenjo_events::{Response, StreamEvent};
    use nenjo_models::{
        ChatMessage, ChatRequest as ModelChatRequest, ChatResponse, TokenUsage, ToolCall,
    };
    use serde_json::Value;
    use uuid::Uuid;

    use crate::external_mcp::ExternalMcpPool;
    use crate::skills::SkillRegistry;
    use crate::tools::platform_services::PlatformToolServices;
    use crate::tools::{NativeRuntime, SecurityPolicy, WorkerToolFactory};

    use super::*;

    type ModelRequests = Arc<Mutex<Vec<Vec<ChatMessage>>>>;
    type ScriptedResponses = Arc<Mutex<VecDeque<ChatResponse>>>;

    struct ScriptedModelProvider {
        requests: ModelRequests,
        responses: ScriptedResponses,
    }

    #[async_trait]
    impl ModelProvider for ScriptedModelProvider {
        async fn chat(
            &self,
            request: ModelChatRequest<'_>,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<ChatResponse> {
            self.requests
                .lock()
                .unwrap()
                .push(request.messages.to_vec());
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| anyhow::anyhow!("scripted model response exhausted"))
        }
    }

    struct ScriptedModelFactory {
        requests: ModelRequests,
        responses: ScriptedResponses,
    }

    impl ModelProviderFactory for ScriptedModelFactory {
        fn create(&self, _provider_name: &str) -> anyhow::Result<Arc<dyn ModelProvider>> {
            Ok(Arc::new(ScriptedModelProvider {
                requests: self.requests.clone(),
                responses: self.responses.clone(),
            }))
        }
    }

    struct WorkspaceToolFactory {
        workspace_dir: PathBuf,
    }

    #[async_trait]
    impl ToolFactory for WorkspaceToolFactory {
        async fn create_tools(&self, _agent: &AgentManifest) -> Vec<Arc<dyn Tool>> {
            Vec::new()
        }

        fn workspace_dir(&self) -> PathBuf {
            self.workspace_dir.clone()
        }
    }

    #[derive(Default)]
    struct CapturedResponses {
        responses: Mutex<Vec<Response>>,
    }

    impl crate::handlers::ResponseSender for CapturedResponses {
        fn send(&self, response: Response) -> anyhow::Result<()> {
            self.responses.lock().unwrap().push(response);
            Ok(())
        }
    }

    fn assistant_delta(run_id: &str, request_id: &str, delta: &str) -> StreamEvent {
        StreamEvent::AssistantTextDelta {
            run_id: run_id.to_string(),
            request_id: request_id.to_string(),
            payload: Some(serde_json::json!({ "delta": delta })),
            encrypted_payload: None,
        }
    }

    fn model_completed(run_id: &str, request_id: &str) -> StreamEvent {
        StreamEvent::ModelRequestCompleted {
            run_id: run_id.to_string(),
            request_id: request_id.to_string(),
            parent_call_id: None,
        }
    }

    fn assistant_delta_text(event: &StreamEvent) -> &str {
        let StreamEvent::AssistantTextDelta { payload, .. } = event else {
            panic!("expected assistant text delta");
        };
        payload
            .as_ref()
            .and_then(|value| value.get("delta"))
            .and_then(serde_json::Value::as_str)
            .expect("assistant delta payload should include delta")
    }

    #[test]
    fn assistant_delta_buffer_coalesces_small_deltas() {
        let session_id = Uuid::new_v4();
        let mut buffer = AssistantDeltaBuffer::default();
        let now = Instant::now();

        assert!(
            buffer
                .push(session_id, assistant_delta("run", "request", "hello "), now)
                .is_empty()
        );
        assert!(
            buffer
                .push(
                    session_id,
                    assistant_delta("run", "request", "world"),
                    now + Duration::from_millis(10),
                )
                .is_empty()
        );

        let events = buffer.flush();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0, session_id);
        assert_eq!(assistant_delta_text(&events[0].1), "hello world");
    }

    #[test]
    fn assistant_delta_buffer_sets_time_based_flush_deadline() {
        let session_id = Uuid::new_v4();
        let mut buffer = AssistantDeltaBuffer::default();
        let now = Instant::now();

        assert!(
            buffer
                .push(
                    session_id,
                    assistant_delta("run", "request", "partial"),
                    now
                )
                .is_empty()
        );

        assert_eq!(
            buffer.next_flush_at(),
            Some(now + ASSISTANT_DELTA_FLUSH_AFTER)
        );
    }

    #[test]
    fn assistant_delta_buffer_flushes_before_non_delta_event() {
        let session_id = Uuid::new_v4();
        let mut buffer = AssistantDeltaBuffer::default();
        let now = Instant::now();

        assert!(
            buffer
                .push(
                    session_id,
                    assistant_delta("run", "request", "partial"),
                    now
                )
                .is_empty()
        );

        let events = buffer.push(
            session_id,
            model_completed("run", "request"),
            now + Duration::from_millis(10),
        );

        assert_eq!(events.len(), 2);
        assert_eq!(assistant_delta_text(&events[0].1), "partial");
        assert!(matches!(
            events[1].1,
            StreamEvent::ModelRequestCompleted { .. }
        ));
    }

    #[test]
    fn assistant_delta_buffer_flushes_when_size_threshold_is_reached() {
        let session_id = Uuid::new_v4();
        let mut buffer = AssistantDeltaBuffer::default();
        let now = Instant::now();
        let large_delta = "x".repeat(ASSISTANT_DELTA_FLUSH_CHARS);

        let events = buffer.push(
            session_id,
            assistant_delta("run", "request", &large_delta),
            now,
        );

        assert_eq!(events.len(), 1);
        assert_eq!(assistant_delta_text(&events[0].1), large_delta);
        assert!(buffer.next_flush_at().is_none());
    }

    #[test]
    fn assistant_progress_response_gets_durable_chat_identity() {
        let input_message_id = Uuid::new_v4();
        let mut event = StreamEvent::AssistantResponse {
            run_id: "run-1".to_string(),
            payload: Some(serde_json::json!({
                "message": "Still working",
                "status": "in_progress",
            })),
            encrypted_payload: None,
        };

        bind_chat_response_context(&mut event, "run-1", Some(input_message_id));

        let StreamEvent::AssistantResponse {
            payload: Some(payload),
            ..
        } = event
        else {
            panic!("expected assistant response payload");
        };
        assert_eq!(payload["input_message_id"], input_message_id.to_string());
        assert!(
            payload["message_id"]
                .as_str()
                .and_then(|value| Uuid::parse_str(value).ok())
                .is_some()
        );
    }

    #[test]
    fn package_command_template_strips_frontmatter_and_expands_arguments() {
        let command = CommandManifest {
            name: "Ralph Loop".to_string(),
            slug: Slug::derive("ralph-loop"),
            path: "plugins/ralph_loop".to_string(),
            command: "/ralph-loop".to_string(),
            description: None,
            entry_path: "command.md".to_string(),
            content: String::new(),
            root_path: "commands/ralph-loop".to_string(),
            root_dir: PathBuf::from("/tmp/commands"),
            plugin_root_path: Some(".".to_string()),
            plugin_root_dir: Some(PathBuf::from("/tmp/plugin")),
            hooks: Vec::new(),
            source_type: "package".to_string(),
            read_only: true,
            metadata: Value::Null,
        };
        let loaded = LoadedCommand {
            markdown: "---\nargument-hint: TASK\n---\nUse $ARGUMENTS with {{ chat.message }}."
                .to_string(),
            source_file: "commands/ralph-loop.md".to_string(),
            command_dir: "/tmp/commands".to_string(),
            plugin_root: "/tmp/plugin".to_string(),
        };

        let template = command_chat_template(
            &command,
            "/ralph-loop",
            "/ralph-loop copy the demo repo",
            &loaded,
        );

        assert_eq!(template, "Use copy the demo repo with {{ chat.message }}.");
    }

    #[test]
    fn native_command_template_keeps_content_unmodified_except_arguments() {
        let command = CommandManifest {
            name: "design".to_string(),
            slug: Slug::derive("design"),
            path: String::new(),
            command: "/design".to_string(),
            description: None,
            entry_path: "command.md".to_string(),
            content: String::new(),
            root_path: String::new(),
            root_dir: PathBuf::new(),
            plugin_root_path: None,
            plugin_root_dir: None,
            hooks: Vec::new(),
            source_type: "native".to_string(),
            read_only: false,
            metadata: Value::Null,
        };
        let loaded = LoadedCommand {
            markdown:
                "---\nnot-frontmatter-for-native\n---\nUse {{ chat.message }} and $ARGUMENTS."
                    .to_string(),
            source_file: "command.md".to_string(),
            command_dir: String::new(),
            plugin_root: String::new(),
        };

        let template = command_chat_template(&command, "/design", "/design a workflow", &loaded);

        assert_eq!(
            template,
            "---\nnot-frontmatter-for-native\n---\nUse {{ chat.message }} and a workflow."
        );
    }

    #[tokio::test]
    async fn load_command_prefers_inline_content_over_runtime_file_paths() {
        let command = CommandManifest {
            name: "design".to_string(),
            slug: Slug::derive("design"),
            path: String::new(),
            command: "/design".to_string(),
            description: None,
            entry_path: "command.md".to_string(),
            content: "Inline command body.".to_string(),
            root_path: "commands/design".to_string(),
            root_dir: PathBuf::from("/tmp/does-not-exist"),
            plugin_root_path: None,
            plugin_root_dir: None,
            hooks: Vec::new(),
            source_type: "package".to_string(),
            read_only: true,
            metadata: Value::Null,
        };

        let loaded = load_command(&command)
            .await
            .expect("inline content should not read from root_dir");

        assert_eq!(loaded.markdown, "Inline command body.");
        assert_eq!(loaded.source_file, "command.md");
    }

    #[tokio::test]
    async fn slash_command_activates_command_hooks_and_uses_state_transcripts() {
        let temp = tempfile::tempdir().unwrap();
        let workspace_dir = temp.path().join("workspace");
        let project_work_dir = workspace_dir.join("demo-project");
        let state_dir = temp.path().join("state");
        let plugin_dir = temp.path().join("packages").join("ralph-loop");
        let command_dir = plugin_dir.join("commands").join("ralph-loop");
        let hooks_dir = plugin_dir.join("hooks");
        tokio::fs::create_dir_all(&project_work_dir).await.unwrap();
        tokio::fs::create_dir_all(&command_dir).await.unwrap();
        tokio::fs::create_dir_all(&hooks_dir).await.unwrap();
        tokio::fs::write(
            command_dir.join("command.md"),
            r#"---
description: Run the Ralph loop workflow.
argument-hint: TASK
---

Use Ralph's loop discipline for $ARGUMENTS.
Original user message: {{ chat.message }}
"#,
        )
        .await
        .unwrap();

        let session_id = Uuid::new_v4();
        let hook_transcript_dir = state_dir
            .join("sessions")
            .join(session_id.to_string())
            .join("hooks");
        tokio::fs::write(
            hooks_dir.join("stop.sh"),
            stop_hook_script(&project_work_dir, &hook_transcript_dir, &plugin_dir),
        )
        .await
        .unwrap();

        let (model_requests, model_responses) =
            scripted_model(vec![text_response("assistant-final")]);
        let manifest = ralph_loop_manifest(&plugin_dir, &command_dir);
        let provider = Provider::builder()
            .with_manifest(manifest)
            .with_model_factory(ScriptedModelFactory {
                requests: model_requests.clone(),
                responses: model_responses,
            })
            .with_tool_factory(WorkspaceToolFactory {
                workspace_dir: workspace_dir.clone(),
            })
            .build()
            .await
            .unwrap();
        let harness = Harness::builder(provider).build();
        let response_sink = Arc::new(CapturedResponses::default());
        let ctx = ChatCommandContext {
            response_sink: response_sink.clone(),
            worker_id: "worker-test".to_string(),
            state_dir: state_dir.clone(),
        };
        let input_message_id = Uuid::new_v4();
        let input_message_id_text = input_message_id.to_string();

        harness
            .handle_chat_command(
                &ctx,
                ChatSlashCommandRequest {
                    message_id: Some(&input_message_id_text),
                    command: "/ralph-loop",
                    content: "/ralph-loop copy the demo repo",
                    project: Some("demo-project"),
                    agent: Some("coder"),
                    target_type: None,
                    target: None,
                    session_id,
                    domain_session_id: None,
                    domain_activation: None,
                },
            )
            .await
            .unwrap();

        let responses = response_sink.responses.lock().unwrap().clone();
        let run_id = responses
            .iter()
            .find_map(|response| match response {
                Response::AgentResponse {
                    payload:
                        StreamEvent::RunStarted {
                            run_id,
                            input_message_id: Some(event_input_message_id),
                            ..
                        },
                    ..
                } if *event_input_message_id == input_message_id => Some(run_id.clone()),
                _ => None,
            })
            .expect("chat run should retain its input message identity");
        assert!(responses.iter().any(|response| matches!(
            response,
            Response::AgentResponse {
                payload: StreamEvent::Done {
                    run_id: Some(done_run_id),
                    input_message_id: Some(done_input_message_id),
                    ..
                },
                ..
            } if done_run_id == &run_id && *done_input_message_id == input_message_id
        )));
        assert_eq!(
            count_hook_events(&responses, HookStreamKind::Started, "Stop", "command"),
            1,
            "command hook activation should be emitted once"
        );
        assert_eq!(
            count_hook_events(&responses, HookStreamKind::Started, "Stop", "command"),
            1,
            "Stop hook should start once"
        );
        assert_eq!(
            count_hook_events(&responses, HookStreamKind::Completed, "Stop", "command"),
            1,
            "Stop hook should complete once"
        );
        assert!(
            hook_completed_successfully(&responses, "Stop", "command"),
            "Stop hook should succeed and expose its stdout preview"
        );
        assert!(
            responses.iter().any(|response| matches!(
                response,
                Response::AgentResponse {
                    payload: StreamEvent::Done { .. },
                    ..
                }
            )),
            "chat command should still finish the normal stream"
        );

        let transcript_path = hook_transcript_dir.join(format!("{session_id}.jsonl"));
        let transcript = tokio::fs::read_to_string(&transcript_path).await.unwrap();
        assert!(transcript.contains("assistant-final"));
        assert!(
            !project_work_dir.join(".nenjo").join("hooks").exists(),
            "hook transcripts should be routed to worker state, not project files"
        );

        let requests = model_requests.lock().unwrap();
        let messages = requests.first().expect("model should be called");
        let rendered_user_message = messages
            .iter()
            .find(|message| {
                message.role == "user" && message.content.contains("Use Ralph's loop discipline")
            })
            .expect("rendered command should be sent as the user message");
        assert!(
            rendered_user_message
                .content
                .contains("for copy the demo repo.")
        );
        assert!(
            rendered_user_message
                .content
                .contains("Original user message: copy the demo repo")
        );
        assert!(
            !rendered_user_message
                .content
                .contains("Original user message: /ralph-loop")
        );
        assert!(!rendered_user_message.content.contains("argument-hint"));
        assert!(
            !rendered_user_message
                .content
                .contains("Installed slash command invocation")
        );
    }

    #[tokio::test]
    async fn use_skill_activates_skill_hooks_for_current_turn() {
        let temp = tempfile::tempdir().unwrap();
        let workspace_dir = temp.path().join("workspace");
        let project_work_dir = workspace_dir.join("demo-project");
        let state_dir = temp.path().join("state");
        let plugin_dir = workspace_dir
            .join(".nenjo")
            .join("plugins")
            .join("ralph-loop");
        let skill_dir = plugin_dir.join("skills").join("ralph-loop");
        let hooks_dir = plugin_dir.join("hooks");
        tokio::fs::create_dir_all(&project_work_dir).await.unwrap();
        tokio::fs::create_dir_all(&skill_dir).await.unwrap();
        tokio::fs::create_dir_all(&hooks_dir).await.unwrap();
        tokio::fs::write(
            skill_dir.join("SKILL.md"),
            "# Ralph Loop\n\nUse the loop until the task is complete.",
        )
        .await
        .unwrap();

        let session_id = Uuid::new_v4();
        let hook_transcript_dir = state_dir
            .join("sessions")
            .join(session_id.to_string())
            .join("hooks");
        tokio::fs::write(
            hooks_dir.join("stop.sh"),
            skill_stop_hook_script(
                &project_work_dir,
                &hook_transcript_dir,
                &plugin_dir,
                &skill_dir,
            ),
        )
        .await
        .unwrap();

        let skill = SkillManifest {
            name: "ralph-loop".to_string(),
            slug: Slug::derive("ralph-loop"),
            aliases: vec!["ralph".to_string()],
            description: Some("Loop until completion.".to_string()),
            entry_path: "SKILL.md".to_string(),
            root_path: "skills/ralph-loop".to_string(),
            root_dir: skill_dir.clone(),
            plugin_root_path: Some(".".to_string()),
            plugin_root_dir: Some(plugin_dir.clone()),
            scripts: Vec::new(),
            references: Vec::new(),
            assets: Vec::new(),
            mcp_servers: Vec::new(),
            hooks: vec![Slug::derive("ralph-loop-stop")],
            source_type: "package".to_string(),
            read_only: true,
            metadata: Value::Null,
        };
        let hook = HookManifest {
            name: "Ralph Loop Stop".to_string(),
            slug: Slug::derive("ralph-loop-stop"),
            description: None,
            event: "Stop".to_string(),
            matcher: Some("*".to_string()),
            hook_type: "command".to_string(),
            command: Some(HookCommandManifest {
                path: "hooks/stop.sh".to_string(),
                args: Vec::new(),
            }),
            timeout_seconds: Some(5),
            plugin_root_path: Some(".".to_string()),
            plugin_root_dir: Some(plugin_dir.clone()),
            source_type: "package".to_string(),
            read_only: true,
            metadata: Value::Null,
        };
        let registry = Arc::new(SkillRegistry::default());
        registry.reconcile(std::slice::from_ref(&skill), std::slice::from_ref(&hook));

        let (model_requests, model_responses) = scripted_model(vec![
            tool_call_response(ToolCall {
                id: "call_use_skill".to_string(),
                name: "use_skill".to_string(),
                arguments: serde_json::json!({ "name": "ralph-loop" }).to_string(),
            }),
            text_response("skill-final"),
        ]);
        let manifest = skill_test_manifest(skill, hook);
        let security = SecurityPolicy::with_workspace_dir(workspace_dir.clone());
        let config = crate::config::Config {
            workspace_dir: workspace_dir.clone(),
            state_dir: state_dir.clone(),
            manifests_dir: temp.path().join("manifests"),
            ..Default::default()
        };
        let tool_factory = WorkerToolFactory::with_skill_registry(
            security,
            NativeRuntime,
            config,
            PlatformToolServices {
                manifest_backend: None,
                task_backend: None,
                ..Default::default()
            },
            Arc::new(ExternalMcpPool::new()),
            registry,
        );
        let provider = Provider::builder()
            .with_manifest(manifest)
            .with_model_factory(ScriptedModelFactory {
                requests: model_requests.clone(),
                responses: model_responses,
            })
            .with_tool_factory(tool_factory)
            .build()
            .await
            .unwrap();
        let harness = Harness::builder(provider).build();
        let response_sink = Arc::new(CapturedResponses::default());
        let ctx = ChatCommandContext {
            response_sink: response_sink.clone(),
            worker_id: "worker-test".to_string(),
            state_dir: state_dir.clone(),
        };

        harness
            .handle_chat(
                &ctx,
                ChatCommandRequest {
                    message_id: None,
                    content: "Use the Ralph Loop skill for this task.",
                    project: Some("demo-project"),
                    agent: Some("coder"),
                    target_type: None,
                    target: None,
                    session_id,
                    domain_session_id: None,
                    domain_activation: None,
                    template_override: None,
                    hook_scopes: Vec::new(),
                },
            )
            .await
            .unwrap();

        let responses = response_sink.responses.lock().unwrap().clone();
        assert_eq!(
            count_hook_events(&responses, HookStreamKind::Started, "Stop", "skill"),
            1,
            "use_skill should emit one skill hook activation"
        );
        assert_eq!(
            count_hook_events(&responses, HookStreamKind::Started, "Stop", "skill"),
            1,
            "activated skill Stop hook should start once"
        );
        assert_eq!(
            count_hook_events(&responses, HookStreamKind::Completed, "Stop", "skill"),
            1,
            "activated skill Stop hook should complete once"
        );
        assert!(
            hook_completed_successfully(&responses, "Stop", "skill"),
            "skill Stop hook should succeed and expose its stdout preview"
        );

        let transcript_path = hook_transcript_dir.join(format!("{session_id}.jsonl"));
        let transcript = tokio::fs::read_to_string(&transcript_path).await.unwrap();
        assert!(transcript.contains("skill-final"));
        assert!(
            !project_work_dir.join(".nenjo").join("hooks").exists(),
            "skill hook transcripts should be routed to worker state"
        );

        let requests = model_requests.lock().unwrap();
        assert_eq!(
            requests.len(),
            2,
            "use_skill should require a second model turn"
        );
        assert!(
            requests[1]
                .iter()
                .any(|message| message.content.contains("--- SKILL.md ---")),
            "loaded skill markdown should be returned to the model after use_skill"
        );
    }

    #[tokio::test]
    async fn use_skill_activates_prompt_tool_and_stop_hooks_for_current_turn() {
        let temp = tempfile::tempdir().unwrap();
        let workspace_dir = temp.path().join("workspace");
        let project_work_dir = workspace_dir.join("demo-project");
        let state_dir = temp.path().join("state");
        let plugin_dir = workspace_dir
            .join(".nenjo")
            .join("plugins")
            .join("ralph-loop");
        let skill_dir = plugin_dir.join("skills").join("ralph-loop");
        let hooks_dir = plugin_dir.join("hooks");
        tokio::fs::create_dir_all(&project_work_dir).await.unwrap();
        tokio::fs::create_dir_all(&skill_dir).await.unwrap();
        tokio::fs::create_dir_all(&hooks_dir).await.unwrap();
        tokio::fs::write(
            skill_dir.join("SKILL.md"),
            "# Ralph Loop\n\nUse the loop until the task is complete.",
        )
        .await
        .unwrap();

        let session_id = Uuid::new_v4();
        let hook_transcript_dir = state_dir
            .join("sessions")
            .join(session_id.to_string())
            .join("hooks");
        tokio::fs::write(
            hooks_dir.join("prompt.sh"),
            skill_user_prompt_hook_script(
                &project_work_dir,
                &hook_transcript_dir,
                &plugin_dir,
                &skill_dir,
                "Use the Ralph Loop skill",
                "skill-prompt-context",
            ),
        )
        .await
        .unwrap();
        tokio::fs::write(
            hooks_dir.join("pre.sh"),
            skill_tool_hook_script(
                &project_work_dir,
                &hook_transcript_dir,
                &plugin_dir,
                &skill_dir,
                "PreToolUse",
                "write",
            ),
        )
        .await
        .unwrap();
        tokio::fs::write(
            hooks_dir.join("post.sh"),
            skill_tool_hook_script(
                &project_work_dir,
                &hook_transcript_dir,
                &plugin_dir,
                &skill_dir,
                "PostToolUse",
                "write",
            ),
        )
        .await
        .unwrap();
        tokio::fs::write(
            hooks_dir.join("stop.sh"),
            skill_stop_hook_script(
                &project_work_dir,
                &hook_transcript_dir,
                &plugin_dir,
                &skill_dir,
            ),
        )
        .await
        .unwrap();

        let skill = SkillManifest {
            name: "ralph-loop".to_string(),
            slug: Slug::derive("ralph-loop"),
            aliases: vec!["ralph".to_string()],
            description: Some("Loop until completion.".to_string()),
            entry_path: "SKILL.md".to_string(),
            root_path: "skills/ralph-loop".to_string(),
            root_dir: skill_dir.clone(),
            plugin_root_path: Some(".".to_string()),
            plugin_root_dir: Some(plugin_dir.clone()),
            scripts: Vec::new(),
            references: Vec::new(),
            assets: Vec::new(),
            mcp_servers: Vec::new(),
            hooks: vec![
                Slug::derive("ralph-loop-prompt"),
                Slug::derive("ralph-loop-pre"),
                Slug::derive("ralph-loop-post"),
                Slug::derive("ralph-loop-stop"),
            ],
            source_type: "package".to_string(),
            read_only: true,
            metadata: Value::Null,
        };
        let hooks = vec![
            skill_hook_manifest(
                &plugin_dir,
                "ralph-loop-prompt",
                "UserPromptSubmit",
                "prompt.sh",
            ),
            skill_hook_manifest(&plugin_dir, "ralph-loop-pre", "PreToolUse", "pre.sh"),
            skill_hook_manifest(&plugin_dir, "ralph-loop-post", "PostToolUse", "post.sh"),
            skill_hook_manifest(&plugin_dir, "ralph-loop-stop", "Stop", "stop.sh"),
        ];
        let registry = Arc::new(SkillRegistry::default());
        registry.reconcile(std::slice::from_ref(&skill), &hooks);

        let (model_requests, model_responses) = scripted_model(vec![
            tool_call_response(ToolCall {
                id: "call_use_skill".to_string(),
                name: "use_skill".to_string(),
                arguments: serde_json::json!({ "name": "ralph-loop" }).to_string(),
            }),
            tool_call_response(ToolCall {
                id: "call_write".to_string(),
                name: "write".to_string(),
                arguments: serde_json::json!({
                    "path": "notes.txt",
                    "content": "done"
                })
                .to_string(),
            }),
            text_response("skill-final"),
        ]);
        let manifest = skill_test_manifest_with_hooks(skill, hooks);
        let security = SecurityPolicy::with_workspace_dir(workspace_dir.clone());
        let config = crate::config::Config {
            workspace_dir: workspace_dir.clone(),
            state_dir: state_dir.clone(),
            manifests_dir: temp.path().join("manifests"),
            ..Default::default()
        };
        let tool_factory = WorkerToolFactory::with_skill_registry(
            security,
            NativeRuntime,
            config,
            PlatformToolServices {
                manifest_backend: None,
                task_backend: None,
                ..Default::default()
            },
            Arc::new(ExternalMcpPool::new()),
            registry,
        );
        let provider = Provider::builder()
            .with_manifest(manifest)
            .with_model_factory(ScriptedModelFactory {
                requests: model_requests.clone(),
                responses: model_responses,
            })
            .with_tool_factory(tool_factory)
            .build()
            .await
            .unwrap();
        let harness = Harness::builder(provider).build();
        let response_sink = Arc::new(CapturedResponses::default());
        let ctx = ChatCommandContext {
            response_sink: response_sink.clone(),
            worker_id: "worker-test".to_string(),
            state_dir: state_dir.clone(),
        };

        harness
            .handle_chat(
                &ctx,
                ChatCommandRequest {
                    message_id: None,
                    content: "Use the Ralph Loop skill and write a note.",
                    project: Some("demo-project"),
                    agent: Some("coder"),
                    target_type: None,
                    target: None,
                    session_id,
                    domain_session_id: None,
                    domain_activation: None,
                    template_override: None,
                    hook_scopes: Vec::new(),
                },
            )
            .await
            .unwrap();

        let responses = response_sink.responses.lock().unwrap().clone();
        for event in ["UserPromptSubmit", "PreToolUse", "PostToolUse", "Stop"] {
            assert_eq!(
                count_hook_events(&responses, HookStreamKind::Started, event, "skill"),
                1,
                "{event} hook activation should be emitted once"
            );
            assert_eq!(
                count_hook_events(&responses, HookStreamKind::Started, event, "skill"),
                1,
                "{event} hook should start once"
            );
            assert_eq!(
                count_hook_events(&responses, HookStreamKind::Completed, event, "skill"),
                1,
                "{event} hook should complete once"
            );
            assert!(
                hook_completed_successfully(&responses, event, "skill"),
                "{event} hook should succeed"
            );
        }

        let transcript_path = hook_transcript_dir.join(format!("{session_id}.jsonl"));
        let transcript = tokio::fs::read_to_string(&transcript_path).await.unwrap();
        assert!(transcript.contains("skill-final"));
        assert!(
            tokio::fs::read_to_string(project_work_dir.join("notes.txt"))
                .await
                .unwrap()
                .contains("done")
        );

        let requests = model_requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        assert!(
            requests[1]
                .iter()
                .any(|message| message.content.contains("skill-prompt-context")),
            "newly activated skill UserPromptSubmit context should be visible before the second model call"
        );
    }

    #[tokio::test]
    async fn pre_tool_use_skill_hook_blocks_matching_tool_without_execution() {
        let temp = tempfile::tempdir().unwrap();
        let workspace_dir = temp.path().join("workspace");
        let project_work_dir = workspace_dir.join("demo-project");
        let state_dir = temp.path().join("state");
        let plugin_dir = workspace_dir
            .join(".nenjo")
            .join("plugins")
            .join("ralph-loop");
        let skill_dir = plugin_dir.join("skills").join("ralph-loop");
        let hooks_dir = plugin_dir.join("hooks");
        tokio::fs::create_dir_all(&project_work_dir).await.unwrap();
        tokio::fs::create_dir_all(&skill_dir).await.unwrap();
        tokio::fs::create_dir_all(&hooks_dir).await.unwrap();
        tokio::fs::write(
            skill_dir.join("SKILL.md"),
            "# Ralph Loop\n\nUse the loop until the task is complete.",
        )
        .await
        .unwrap();

        let session_id = Uuid::new_v4();
        let hook_transcript_dir = state_dir
            .join("sessions")
            .join(session_id.to_string())
            .join("hooks");
        tokio::fs::write(
            hooks_dir.join("pre_block.sh"),
            skill_pre_block_hook_script(
                &project_work_dir,
                &hook_transcript_dir,
                &plugin_dir,
                &skill_dir,
            ),
        )
        .await
        .unwrap();

        let skill =
            ralph_loop_skill_manifest(&plugin_dir, &skill_dir, vec!["ralph-loop-pre-block"]);
        let hook = skill_hook_manifest_with_matcher(
            &plugin_dir,
            "ralph-loop-pre-block",
            "PreToolUse",
            "pre_block.sh",
            "write",
        );
        let registry = Arc::new(SkillRegistry::default());
        registry.reconcile(std::slice::from_ref(&skill), std::slice::from_ref(&hook));

        let (model_requests, model_responses) = scripted_model(vec![
            tool_call_response(ToolCall {
                id: "call_use_skill".to_string(),
                name: "use_skill".to_string(),
                arguments: serde_json::json!({ "name": "ralph-loop" }).to_string(),
            }),
            tool_call_response(ToolCall {
                id: "call_blocked_write".to_string(),
                name: "write".to_string(),
                arguments: serde_json::json!({
                    "path": "blocked.txt",
                    "content": "this should not be written"
                })
                .to_string(),
            }),
            text_response("blocked-final"),
        ]);
        let manifest = skill_test_manifest(skill, hook);
        let security = SecurityPolicy::with_workspace_dir(workspace_dir.clone());
        let config = crate::config::Config {
            workspace_dir: workspace_dir.clone(),
            state_dir: state_dir.clone(),
            manifests_dir: temp.path().join("manifests"),
            ..Default::default()
        };
        let tool_factory = WorkerToolFactory::with_skill_registry(
            security,
            NativeRuntime,
            config,
            PlatformToolServices {
                manifest_backend: None,
                task_backend: None,
                ..Default::default()
            },
            Arc::new(ExternalMcpPool::new()),
            registry,
        );
        let provider = Provider::builder()
            .with_manifest(manifest)
            .with_model_factory(ScriptedModelFactory {
                requests: model_requests.clone(),
                responses: model_responses,
            })
            .with_tool_factory(tool_factory)
            .build()
            .await
            .unwrap();
        let harness = Harness::builder(provider).build();
        let response_sink = Arc::new(CapturedResponses::default());
        let ctx = ChatCommandContext {
            response_sink: response_sink.clone(),
            worker_id: "worker-test".to_string(),
            state_dir: state_dir.clone(),
        };

        harness
            .handle_chat(
                &ctx,
                ChatCommandRequest {
                    message_id: None,
                    content: "Use the Ralph Loop skill and write a blocked file.",
                    project: Some("demo-project"),
                    agent: Some("coder"),
                    target_type: None,
                    target: None,
                    session_id,
                    domain_session_id: None,
                    domain_activation: None,
                    template_override: None,
                    hook_scopes: Vec::new(),
                },
            )
            .await
            .unwrap();

        let responses = response_sink.responses.lock().unwrap().clone();
        assert_eq!(
            count_hook_events(&responses, HookStreamKind::Started, "PreToolUse", "skill"),
            1,
            "use_skill should emit one PreToolUse hook activation"
        );
        assert_eq!(
            count_hook_events(&responses, HookStreamKind::Started, "PreToolUse", "skill"),
            1,
            "PreToolUse hook should start once"
        );
        assert_eq!(
            count_hook_events(&responses, HookStreamKind::Completed, "PreToolUse", "skill"),
            1,
            "PreToolUse hook should complete once"
        );
        assert!(
            hook_completed_blocked(&responses, "PreToolUse", "skill", "no writes"),
            "PreToolUse hook should report a blocked decision with the hook reason"
        );
        assert!(
            !project_work_dir.join("blocked.txt").exists(),
            "blocked write must not execute after a PreToolUse block"
        );

        let requests = model_requests.lock().unwrap();
        assert_eq!(
            requests.len(),
            3,
            "blocked tool result should be returned to the model"
        );
        assert!(
            requests[2].iter().any(|message| {
                message.content.contains("Blocked by hook") && message.content.contains("no writes")
            }),
            "model should receive the PreToolUse block as the failed tool result"
        );
    }

    #[tokio::test]
    async fn post_tool_use_skill_hook_receives_success_and_error_response_shapes() {
        let temp = tempfile::tempdir().unwrap();
        let workspace_dir = temp.path().join("workspace");
        let project_work_dir = workspace_dir.join("demo-project");
        let state_dir = temp.path().join("state");
        let plugin_dir = workspace_dir
            .join(".nenjo")
            .join("plugins")
            .join("ralph-loop");
        let skill_dir = plugin_dir.join("skills").join("ralph-loop");
        let hooks_dir = plugin_dir.join("hooks");
        tokio::fs::create_dir_all(&project_work_dir).await.unwrap();
        tokio::fs::create_dir_all(&skill_dir).await.unwrap();
        tokio::fs::create_dir_all(&hooks_dir).await.unwrap();
        tokio::fs::write(
            skill_dir.join("SKILL.md"),
            "# Ralph Loop\n\nUse the loop until the task is complete.",
        )
        .await
        .unwrap();

        let session_id = Uuid::new_v4();
        let hook_transcript_dir = state_dir
            .join("sessions")
            .join(session_id.to_string())
            .join("hooks");
        tokio::fs::write(
            hooks_dir.join("post_write.sh"),
            skill_post_response_hook_script(
                &project_work_dir,
                &hook_transcript_dir,
                &plugin_dir,
                &skill_dir,
                "write",
                true,
            ),
        )
        .await
        .unwrap();
        tokio::fs::write(
            hooks_dir.join("post_read.sh"),
            skill_post_response_hook_script(
                &project_work_dir,
                &hook_transcript_dir,
                &plugin_dir,
                &skill_dir,
                "read",
                false,
            ),
        )
        .await
        .unwrap();

        let skill = ralph_loop_skill_manifest(
            &plugin_dir,
            &skill_dir,
            vec!["ralph-loop-post-write", "ralph-loop-post-read"],
        );
        let hooks = vec![
            skill_hook_manifest_with_matcher(
                &plugin_dir,
                "ralph-loop-post-write",
                "PostToolUse",
                "post_write.sh",
                "write",
            ),
            skill_hook_manifest_with_matcher(
                &plugin_dir,
                "ralph-loop-post-read",
                "PostToolUse",
                "post_read.sh",
                "read",
            ),
        ];
        let registry = Arc::new(SkillRegistry::default());
        registry.reconcile(std::slice::from_ref(&skill), &hooks);

        let (model_requests, model_responses) = scripted_model(vec![
            tool_call_response(ToolCall {
                id: "call_use_skill".to_string(),
                name: "use_skill".to_string(),
                arguments: serde_json::json!({ "name": "ralph-loop" }).to_string(),
            }),
            tool_call_response(ToolCall {
                id: "call_write".to_string(),
                name: "write".to_string(),
                arguments: serde_json::json!({
                    "path": "notes.txt",
                    "content": "written"
                })
                .to_string(),
            }),
            tool_call_response(ToolCall {
                id: "call_missing_read".to_string(),
                name: "read".to_string(),
                arguments: serde_json::json!({
                    "path": "missing.txt"
                })
                .to_string(),
            }),
            text_response("post-final"),
        ]);
        let manifest = skill_test_manifest_with_hooks(skill, hooks);
        let security = SecurityPolicy::with_workspace_dir(workspace_dir.clone());
        let config = crate::config::Config {
            workspace_dir: workspace_dir.clone(),
            state_dir: state_dir.clone(),
            manifests_dir: temp.path().join("manifests"),
            ..Default::default()
        };
        let tool_factory = WorkerToolFactory::with_skill_registry(
            security,
            NativeRuntime,
            config,
            PlatformToolServices {
                manifest_backend: None,
                task_backend: None,
                ..Default::default()
            },
            Arc::new(ExternalMcpPool::new()),
            registry,
        );
        let provider = Provider::builder()
            .with_manifest(manifest)
            .with_model_factory(ScriptedModelFactory {
                requests: model_requests.clone(),
                responses: model_responses,
            })
            .with_tool_factory(tool_factory)
            .build()
            .await
            .unwrap();
        let harness = Harness::builder(provider).build();
        let response_sink = Arc::new(CapturedResponses::default());
        let ctx = ChatCommandContext {
            response_sink: response_sink.clone(),
            worker_id: "worker-test".to_string(),
            state_dir: state_dir.clone(),
        };

        harness
            .handle_chat(
                &ctx,
                ChatCommandRequest {
                    message_id: None,
                    content: "Use the Ralph Loop skill, write a note, then read a missing file.",
                    project: Some("demo-project"),
                    agent: Some("coder"),
                    target_type: None,
                    target: None,
                    session_id,
                    domain_session_id: None,
                    domain_activation: None,
                    template_override: None,
                    hook_scopes: Vec::new(),
                },
            )
            .await
            .unwrap();

        let responses = response_sink.responses.lock().unwrap().clone();
        assert_eq!(
            count_hook_events(&responses, HookStreamKind::Started, "PostToolUse", "skill"),
            2,
            "use_skill should emit both PostToolUse hook activations"
        );
        assert_eq!(
            count_hook_events(&responses, HookStreamKind::Started, "PostToolUse", "skill"),
            2,
            "PostToolUse hooks should start for success and error tool results"
        );
        assert_eq!(
            count_hook_events(
                &responses,
                HookStreamKind::Completed,
                "PostToolUse",
                "skill"
            ),
            2,
            "PostToolUse hooks should complete for success and error tool results"
        );
        assert_eq!(
            count_successful_hook_completions(&responses, "PostToolUse", "skill"),
            2,
            "PostToolUse hooks should validate both response shapes"
        );
        assert!(
            tokio::fs::read_to_string(project_work_dir.join("notes.txt"))
                .await
                .unwrap()
                .contains("written")
        );

        let requests = model_requests.lock().unwrap();
        assert_eq!(requests.len(), 4);
        assert!(
            requests[3]
                .iter()
                .any(|message| message.content.contains("Failed to resolve file path")),
            "model should receive the failed read result after the PostToolUse hook"
        );
    }

    #[tokio::test]
    async fn use_skill_lists_and_calls_skill_activated_mcp_tools() {
        let temp = tempfile::tempdir().unwrap();
        let workspace_dir = temp.path().join("workspace");
        let project_work_dir = workspace_dir.join("demo-project");
        let state_dir = temp.path().join("state");
        let plugin_dir = workspace_dir
            .join(".nenjo")
            .join("plugins")
            .join("mcp-skill");
        let skill_dir = plugin_dir.join("skills").join("mcp-skill");
        tokio::fs::create_dir_all(&project_work_dir).await.unwrap();
        tokio::fs::create_dir_all(&skill_dir).await.unwrap();
        tokio::fs::write(
            skill_dir.join("SKILL.md"),
            "# MCP Skill\n\nUse the review MCP tool.",
        )
        .await
        .unwrap();
        tokio::fs::write(plugin_dir.join("server.sh"), skill_mcp_fixture_script())
            .await
            .unwrap();

        let mcp_server = skill_mcp_server_manifest(&plugin_dir);
        let skill = SkillManifest {
            name: "mcp-skill".to_string(),
            slug: Slug::derive("mcp-skill"),
            aliases: Vec::new(),
            description: Some("Skill with MCP tools.".to_string()),
            entry_path: "SKILL.md".to_string(),
            root_path: "skills/mcp-skill".to_string(),
            root_dir: skill_dir,
            plugin_root_path: Some(".".to_string()),
            plugin_root_dir: Some(plugin_dir.clone()),
            scripts: Vec::new(),
            references: Vec::new(),
            assets: Vec::new(),
            mcp_servers: vec![mcp_server.slug.clone()],
            hooks: Vec::new(),
            source_type: "package".to_string(),
            read_only: true,
            metadata: Value::Null,
        };
        let registry = Arc::new(SkillRegistry::default());
        registry.reconcile(std::slice::from_ref(&skill), &[]);
        let external_mcp = Arc::new(ExternalMcpPool::new());
        external_mcp
            .reconcile(std::slice::from_ref(&mcp_server))
            .await;

        let (model_requests, model_responses) = scripted_model(vec![
            tool_call_response(ToolCall {
                id: "call_use_skill".to_string(),
                name: "use_skill".to_string(),
                arguments: serde_json::json!({ "name": "mcp-skill" }).to_string(),
            }),
            tool_call_response(ToolCall {
                id: "call_skill_mcp".to_string(),
                name: "call_skill_mcp_tool".to_string(),
                arguments: serde_json::json!({
                    "tool": "review",
                    "arguments": {
                        "topic": "demo"
                    }
                })
                .to_string(),
            }),
            text_response("mcp-done"),
        ]);
        let mut manifest = skill_test_manifest_with_hooks(skill, Vec::new());
        manifest.mcp_servers = vec![mcp_server];
        let security = SecurityPolicy::with_workspace_dir(workspace_dir.clone());
        let config = crate::config::Config {
            workspace_dir: workspace_dir.clone(),
            state_dir: state_dir.clone(),
            manifests_dir: temp.path().join("manifests"),
            ..Default::default()
        };
        let tool_factory = WorkerToolFactory::with_skill_registry(
            security,
            NativeRuntime,
            config,
            PlatformToolServices {
                manifest_backend: None,
                task_backend: None,
                ..Default::default()
            },
            external_mcp,
            registry,
        );
        let provider = Provider::builder()
            .with_manifest(manifest)
            .with_model_factory(ScriptedModelFactory {
                requests: model_requests.clone(),
                responses: model_responses,
            })
            .with_tool_factory(tool_factory)
            .build()
            .await
            .unwrap();
        let harness = Harness::builder(provider).build();
        let response_sink = Arc::new(CapturedResponses::default());
        let ctx = ChatCommandContext {
            response_sink,
            worker_id: "worker-test".to_string(),
            state_dir: state_dir.clone(),
        };

        harness
            .handle_chat(
                &ctx,
                ChatCommandRequest {
                    message_id: None,
                    content: "Use the MCP skill to review the demo project.",
                    project: Some("demo-project"),
                    agent: Some("coder"),
                    target_type: None,
                    target: None,
                    session_id: Uuid::new_v4(),
                    domain_session_id: None,
                    domain_activation: None,
                    template_override: None,
                    hook_scopes: Vec::new(),
                },
            )
            .await
            .unwrap();

        let requests = model_requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        let second_request = &requests[1];
        assert!(
            second_request
                .iter()
                .any(|message| message.content.contains("ACTIVE SKILL MCP TOOLS"))
        );
        assert!(
            second_request
                .iter()
                .any(|message| message.content.contains("call_skill_mcp_tool"))
        );
        assert!(
            second_request
                .iter()
                .any(|message| message.content.contains("tool: `review`"))
        );
        assert!(
            second_request
                .iter()
                .any(|message| message.content.contains("arguments_schema"))
        );
        assert!(
            requests[2]
                .iter()
                .any(|message| message.content.contains("skill-mcp-review-ok:demo")),
            "MCP tool result should be visible to the model after proxy call"
        );
    }

    #[tokio::test]
    async fn user_prompt_submit_command_hook_adds_model_context() {
        let temp = tempfile::tempdir().unwrap();
        let workspace_dir = temp.path().join("workspace");
        let project_work_dir = workspace_dir.join("demo-project");
        let state_dir = temp.path().join("state");
        let plugin_dir = temp.path().join("packages").join("ralph-loop");
        let command_dir = plugin_dir.join("commands").join("ralph-loop");
        let hooks_dir = plugin_dir.join("hooks");
        tokio::fs::create_dir_all(&project_work_dir).await.unwrap();
        tokio::fs::create_dir_all(&command_dir).await.unwrap();
        tokio::fs::create_dir_all(&hooks_dir).await.unwrap();
        tokio::fs::write(command_dir.join("command.md"), "Use the submitted task.")
            .await
            .unwrap();

        let session_id = Uuid::new_v4();
        let hook_transcript_dir = state_dir
            .join("sessions")
            .join(session_id.to_string())
            .join("hooks");
        tokio::fs::write(
            hooks_dir.join("prompt.sh"),
            user_prompt_hook_script(
                &project_work_dir,
                &hook_transcript_dir,
                &plugin_dir,
                "prompt-hook-context",
            ),
        )
        .await
        .unwrap();

        let (model_requests, model_responses) = scripted_model(vec![text_response("done")]);
        let manifest = ralph_loop_manifest_with_hook(
            &plugin_dir,
            &command_dir,
            "UserPromptSubmit",
            "prompt.sh",
        );
        let provider = Provider::builder()
            .with_manifest(manifest)
            .with_model_factory(ScriptedModelFactory {
                requests: model_requests.clone(),
                responses: model_responses,
            })
            .with_tool_factory(WorkspaceToolFactory {
                workspace_dir: workspace_dir.clone(),
            })
            .build()
            .await
            .unwrap();
        let harness = Harness::builder(provider).build();
        let response_sink = Arc::new(CapturedResponses::default());
        let ctx = ChatCommandContext {
            response_sink: response_sink.clone(),
            worker_id: "worker-test".to_string(),
            state_dir: state_dir.clone(),
        };

        harness
            .handle_chat_command(
                &ctx,
                ChatSlashCommandRequest {
                    message_id: None,
                    command: "/ralph-loop",
                    content: "/ralph-loop add prompt context",
                    project: Some("demo-project"),
                    agent: Some("coder"),
                    target_type: None,
                    target: None,
                    session_id,
                    domain_session_id: None,
                    domain_activation: None,
                },
            )
            .await
            .unwrap();

        let responses = response_sink.responses.lock().unwrap().clone();
        assert_eq!(
            count_hook_events(
                &responses,
                HookStreamKind::Started,
                "UserPromptSubmit",
                "command"
            ),
            1,
            "command hook activation should be emitted once"
        );
        assert_eq!(
            count_hook_events(
                &responses,
                HookStreamKind::Started,
                "UserPromptSubmit",
                "command"
            ),
            1,
            "UserPromptSubmit hook should start once"
        );
        assert_eq!(
            count_hook_events(
                &responses,
                HookStreamKind::Completed,
                "UserPromptSubmit",
                "command"
            ),
            1,
            "UserPromptSubmit hook should complete once"
        );
        assert!(
            hook_completed_successfully(&responses, "UserPromptSubmit", "command"),
            "UserPromptSubmit hook should succeed"
        );

        let transcript_path = hook_transcript_dir.join(format!("{session_id}.jsonl"));
        let transcript = tokio::fs::read_to_string(&transcript_path).await.unwrap();
        assert!(transcript.contains("Use the submitted task."));

        let requests = model_requests.lock().unwrap();
        let messages = requests.first().expect("model should be called");
        assert!(
            messages
                .iter()
                .any(|message| message.content.contains("prompt-hook-context")),
            "UserPromptSubmit additionalContext should be visible to the model"
        );
    }

    #[tokio::test]
    async fn stop_hook_request_next_turn_continues_with_hook_guidance() {
        let temp = tempfile::tempdir().unwrap();
        let workspace_dir = temp.path().join("workspace");
        let project_work_dir = workspace_dir.join("demo-project");
        let state_dir = temp.path().join("state");
        let plugin_dir = temp.path().join("packages").join("ralph-loop");
        let command_dir = plugin_dir.join("commands").join("ralph-loop");
        let hooks_dir = plugin_dir.join("hooks");
        tokio::fs::create_dir_all(&project_work_dir).await.unwrap();
        tokio::fs::create_dir_all(&command_dir).await.unwrap();
        tokio::fs::create_dir_all(&hooks_dir).await.unwrap();
        tokio::fs::write(command_dir.join("command.md"), "Use the submitted task.")
            .await
            .unwrap();

        let session_id = Uuid::new_v4();
        let hook_transcript_dir = state_dir
            .join("sessions")
            .join(session_id.to_string())
            .join("hooks");
        tokio::fs::write(
            hooks_dir.join("stop.sh"),
            stop_request_next_turn_hook_script(
                &project_work_dir,
                &hook_transcript_dir,
                &plugin_dir,
                "revised-final",
                "revise before stopping",
                "Use the stop hook guidance.",
            ),
        )
        .await
        .unwrap();

        let (model_requests, model_responses) = scripted_model(vec![
            text_response("draft-final"),
            text_response("revised-final"),
        ]);
        let manifest = ralph_loop_manifest(&plugin_dir, &command_dir);
        let provider = Provider::builder()
            .with_manifest(manifest)
            .with_agent_config(AgentConfig {
                max_turns: 4,
                ..Default::default()
            })
            .with_model_factory(ScriptedModelFactory {
                requests: model_requests.clone(),
                responses: model_responses,
            })
            .with_tool_factory(WorkspaceToolFactory {
                workspace_dir: workspace_dir.clone(),
            })
            .build()
            .await
            .unwrap();
        let harness = Harness::builder(provider).build();
        let response_sink = Arc::new(CapturedResponses::default());
        let ctx = ChatCommandContext {
            response_sink: response_sink.clone(),
            worker_id: "worker-test".to_string(),
            state_dir: state_dir.clone(),
        };

        harness
            .handle_chat_command(
                &ctx,
                ChatSlashCommandRequest {
                    message_id: None,
                    command: "/ralph-loop",
                    content: "/ralph-loop produce the final answer",
                    project: Some("demo-project"),
                    agent: Some("coder"),
                    target_type: None,
                    target: None,
                    session_id,
                    domain_session_id: None,
                    domain_activation: None,
                },
            )
            .await
            .unwrap();

        let responses = response_sink.responses.lock().unwrap().clone();
        assert_eq!(
            count_hook_events(&responses, HookStreamKind::Started, "Stop", "command"),
            2,
            "Stop hook should run for the blocked draft and the accepted revision"
        );
        assert_eq!(
            count_hook_events(&responses, HookStreamKind::Completed, "Stop", "command"),
            2,
            "Stop hook should complete twice"
        );
        assert!(
            hook_completed_blocked(&responses, "Stop", "command", "revise before stopping"),
            "first Stop hook completion should request another turn"
        );
        assert!(
            hook_completed_successfully(&responses, "Stop", "command"),
            "second Stop hook completion should allow the final answer"
        );
        assert!(
            done_output_contains(&responses, "revised-final"),
            "chat should finish with the revised model output"
        );

        let requests = model_requests.lock().unwrap();
        assert_eq!(
            requests.len(),
            2,
            "Stop request_next_turn should trigger one more model request"
        );
        assert!(
            requests[1]
                .iter()
                .any(|message| message.content.contains("Use the stop hook guidance.")),
            "systemMessage should be appended before the continuation request"
        );
        assert!(
            requests[1].iter().any(|message| {
                message
                    .content
                    .contains("Hook `Ralph Loop Stop` blocked completion")
                    && message.content.contains("revise before stopping")
            }),
            "the continuation request should include the hook reason"
        );
    }

    #[tokio::test]
    async fn stop_hook_request_next_turn_fails_at_max_turns() {
        let temp = tempfile::tempdir().unwrap();
        let workspace_dir = temp.path().join("workspace");
        let project_work_dir = workspace_dir.join("demo-project");
        let state_dir = temp.path().join("state");
        let plugin_dir = temp.path().join("packages").join("ralph-loop");
        let command_dir = plugin_dir.join("commands").join("ralph-loop");
        let hooks_dir = plugin_dir.join("hooks");
        tokio::fs::create_dir_all(&project_work_dir).await.unwrap();
        tokio::fs::create_dir_all(&command_dir).await.unwrap();
        tokio::fs::create_dir_all(&hooks_dir).await.unwrap();
        tokio::fs::write(command_dir.join("command.md"), "Use the submitted task.")
            .await
            .unwrap();

        let session_id = Uuid::new_v4();
        let hook_transcript_dir = state_dir
            .join("sessions")
            .join(session_id.to_string())
            .join("hooks");
        tokio::fs::write(
            hooks_dir.join("stop.sh"),
            stop_always_request_next_turn_hook_script(
                &project_work_dir,
                &hook_transcript_dir,
                &plugin_dir,
                "keep going",
            ),
        )
        .await
        .unwrap();

        let (model_requests, model_responses) =
            scripted_model(vec![text_response("draft-1"), text_response("draft-2")]);
        let manifest = ralph_loop_manifest(&plugin_dir, &command_dir);
        let provider = Provider::builder()
            .with_manifest(manifest)
            .with_agent_config(AgentConfig {
                max_turns: 2,
                ..Default::default()
            })
            .with_model_factory(ScriptedModelFactory {
                requests: model_requests.clone(),
                responses: model_responses,
            })
            .with_tool_factory(WorkspaceToolFactory {
                workspace_dir: workspace_dir.clone(),
            })
            .build()
            .await
            .unwrap();
        let harness = Harness::builder(provider).build();
        let response_sink = Arc::new(CapturedResponses::default());
        let ctx = ChatCommandContext {
            response_sink: response_sink.clone(),
            worker_id: "worker-test".to_string(),
            state_dir: state_dir.clone(),
        };

        let error = harness
            .handle_chat_command(
                &ctx,
                ChatSlashCommandRequest {
                    message_id: None,
                    command: "/ralph-loop",
                    content: "/ralph-loop keep trying",
                    project: Some("demo-project"),
                    agent: Some("coder"),
                    target_type: None,
                    target: None,
                    session_id,
                    domain_session_id: None,
                    domain_activation: None,
                },
            )
            .await
            .expect_err("max-turn exhaustion should fail the chat run");

        assert!(
            format!("{error:#}")
                .contains("turn loop reached the maximum of 2 turns without a final response"),
            "the max-turn failure should propagate through the harness: {error:#}"
        );

        let responses = response_sink.responses.lock().unwrap().clone();
        assert_eq!(
            count_hook_events(&responses, HookStreamKind::Started, "Stop", "command"),
            2,
            "Stop hook continuations should stop at max_turns"
        );
        assert_eq!(
            count_hook_events(&responses, HookStreamKind::Completed, "Stop", "command"),
            2,
            "Stop hook should complete for each capped turn"
        );
        assert!(
            hook_completed_blocked(&responses, "Stop", "command", "keep going"),
            "Stop hook should request continuation before the cap is reached"
        );
        assert!(
            !responses.iter().any(|response| matches!(
                response,
                Response::AgentResponse {
                    payload: StreamEvent::Done { .. },
                    ..
                }
            )),
            "max-turn exhaustion must not emit a successful Done output"
        );

        let requests = model_requests.lock().unwrap();
        assert_eq!(
            requests.len(),
            2,
            "the turn loop must not request beyond max_turns"
        );
    }

    fn ralph_loop_manifest(plugin_dir: &Path, command_dir: &Path) -> Manifest {
        ralph_loop_manifest_with_hook(plugin_dir, command_dir, "Stop", "stop.sh")
    }

    fn ralph_loop_manifest_with_hook(
        plugin_dir: &Path,
        command_dir: &Path,
        hook_event: &str,
        script_name: &str,
    ) -> Manifest {
        let model = ModelManifest {
            slug: model_manifest_slug("test", "mock"),
            name: "test-model".to_string(),
            description: None,
            model: "mock".to_string(),
            model_provider: "test".to_string(),
            temperature: Some(0.0),
            context_window: None,
            base_url: None,
            native_tools: vec![],
        };
        let model_slug = model_manifest_slug(&model.model_provider, &model.model);
        Manifest {
            models: vec![model],
            agents: vec![AgentManifest {
                name: "Coder".to_string(),
                slug: Slug::derive("coder"),
                description: None,
                prompt_config: PromptConfig::default(),
                color: None,
                model: Some(model_slug),
                domains: Vec::new(),
                platform_scopes: Vec::new(),
                mcp_servers: Vec::new(),
                script_tools: Vec::new(),
                media: Vec::new(),
                abilities: Vec::new(),
                prompt_locked: false,
                source_type: None,
                metadata: serde_json::json!({}),
            }],
            projects: vec![ProjectManifest {
                name: "Demo Project".to_string(),
                slug: Slug::derive("demo-project"),
                description: None,
                settings: Value::Null,
            }],
            commands: vec![CommandManifest {
                name: "Ralph Loop".to_string(),
                slug: Slug::derive("ralph-loop"),
                path: "plugins/ralph_loop".to_string(),
                command: "/ralph-loop".to_string(),
                description: None,
                entry_path: "command.md".to_string(),
                content: String::new(),
                root_path: "commands/ralph-loop".to_string(),
                root_dir: command_dir.to_path_buf(),
                plugin_root_path: Some(".".to_string()),
                plugin_root_dir: Some(plugin_dir.to_path_buf()),
                hooks: vec![Slug::derive("ralph-loop-stop")],
                source_type: "package".to_string(),
                read_only: true,
                metadata: Value::Null,
            }],
            hooks: vec![HookManifest {
                name: "Ralph Loop Stop".to_string(),
                slug: Slug::derive("ralph-loop-stop"),
                description: None,
                event: hook_event.to_string(),
                matcher: Some("*".to_string()),
                hook_type: "command".to_string(),
                command: Some(HookCommandManifest {
                    path: format!("hooks/{script_name}"),
                    args: Vec::new(),
                }),
                timeout_seconds: Some(5),
                plugin_root_path: Some(".".to_string()),
                plugin_root_dir: Some(plugin_dir.to_path_buf()),
                source_type: "package".to_string(),
                read_only: true,
                metadata: Value::Null,
            }],
            ..Default::default()
        }
    }

    fn skill_test_manifest(skill: SkillManifest, hook: HookManifest) -> Manifest {
        skill_test_manifest_with_hooks(skill, vec![hook])
    }

    fn skill_test_manifest_with_hooks(skill: SkillManifest, hooks: Vec<HookManifest>) -> Manifest {
        let model = ModelManifest {
            slug: model_manifest_slug("test", "mock"),
            name: "test-model".to_string(),
            description: None,
            model: "mock".to_string(),
            model_provider: "test".to_string(),
            temperature: Some(0.0),
            context_window: None,
            base_url: None,
            native_tools: vec![],
        };
        let model_slug = model_manifest_slug(&model.model_provider, &model.model);
        Manifest {
            models: vec![model],
            agents: vec![AgentManifest {
                name: "Coder".to_string(),
                slug: Slug::derive("coder"),
                description: None,
                prompt_config: PromptConfig::default(),
                color: None,
                model: Some(model_slug),
                domains: Vec::new(),
                platform_scopes: Vec::new(),
                mcp_servers: Vec::new(),
                script_tools: Vec::new(),
                media: Vec::new(),
                abilities: Vec::new(),
                prompt_locked: false,
                source_type: None,
                metadata: serde_json::json!({}),
            }],
            projects: vec![ProjectManifest {
                name: "Demo Project".to_string(),
                slug: Slug::derive("demo-project"),
                description: None,
                settings: Value::Null,
            }],
            skills: vec![skill],
            hooks,
            ..Default::default()
        }
    }

    fn ralph_loop_skill_manifest(
        plugin_dir: &Path,
        skill_dir: &Path,
        hook_names: Vec<&str>,
    ) -> SkillManifest {
        SkillManifest {
            name: "ralph-loop".to_string(),
            slug: Slug::derive("ralph-loop"),
            aliases: vec!["ralph".to_string()],
            description: Some("Loop until completion.".to_string()),
            entry_path: "SKILL.md".to_string(),
            root_path: "skills/ralph-loop".to_string(),
            root_dir: skill_dir.to_path_buf(),
            plugin_root_path: Some(".".to_string()),
            plugin_root_dir: Some(plugin_dir.to_path_buf()),
            scripts: Vec::new(),
            references: Vec::new(),
            assets: Vec::new(),
            mcp_servers: Vec::new(),
            hooks: hook_names.into_iter().map(Slug::derive).collect(),
            source_type: "package".to_string(),
            read_only: true,
            metadata: Value::Null,
        }
    }

    fn skill_hook_manifest(
        plugin_dir: &Path,
        name: &str,
        event: &str,
        script_name: &str,
    ) -> HookManifest {
        let matcher = if matches!(event, "PreToolUse" | "PostToolUse") {
            "write"
        } else {
            "*"
        };
        skill_hook_manifest_with_matcher(plugin_dir, name, event, script_name, matcher)
    }

    fn skill_hook_manifest_with_matcher(
        plugin_dir: &Path,
        name: &str,
        event: &str,
        script_name: &str,
        matcher: &str,
    ) -> HookManifest {
        HookManifest {
            name: "Ralph Loop Stop".to_string(),
            slug: Slug::derive(name),
            description: None,
            event: event.to_string(),
            matcher: Some(matcher.to_string()),
            hook_type: "command".to_string(),
            command: Some(HookCommandManifest {
                path: format!("hooks/{script_name}"),
                args: Vec::new(),
            }),
            timeout_seconds: Some(5),
            plugin_root_path: Some(".".to_string()),
            plugin_root_dir: Some(plugin_dir.to_path_buf()),
            source_type: "package".to_string(),
            read_only: true,
            metadata: Value::Null,
        }
    }

    fn skill_mcp_server_manifest(plugin_dir: &Path) -> McpServerManifest {
        McpServerManifest {
            slug: nenjo::Slug::derive("mcp-skill-review-server"),
            name: "MCP Skill: Review Server".to_string(),
            description: Some("Review MCP server".to_string()),
            transport: "stdio".to_string(),
            command: Some("bash".to_string()),
            args: Some(vec!["server.sh".to_string()]),
            url: None,
            env_schema: serde_json::json!([]),
            source_type: "package".to_string(),
            read_only: true,
            metadata: serde_json::json!({
                "runtime": {
                    "cwd": plugin_dir.to_string_lossy().to_string(),
                    "env": {
                        "MODE": "skill"
                    }
                },
                "claude": {
                    "plugin": {
                        "slug": "mcp_skill"
                    },
                    "mcp": {
                        "name": "review-server"
                    }
                }
            }),
        }
    }

    fn scripted_model(responses: Vec<ChatResponse>) -> (ModelRequests, ScriptedResponses) {
        (
            Arc::new(Mutex::new(Vec::new())),
            Arc::new(Mutex::new(VecDeque::from(responses))),
        )
    }

    fn text_response(text: impl Into<String>) -> ChatResponse {
        let text = text.into();
        ChatResponse {
            text: None,
            tool_calls: vec![ToolCall {
                id: "call_respond_to_user".to_string(),
                name: "respond_to_user".to_string(),
                arguments: serde_json::json!({
                    "message": text,
                    "status": "completed",
                })
                .to_string(),
            }],
            provider_tool_calls: vec![],
            usage: TokenUsage::default(),
        }
    }

    fn tool_call_response(tool_call: ToolCall) -> ChatResponse {
        ChatResponse {
            text: None,
            tool_calls: vec![tool_call],
            provider_tool_calls: vec![],
            usage: TokenUsage::default(),
        }
    }

    fn stop_hook_script(
        expected_cwd: &Path,
        expected_transcript_dir: &Path,
        expected_plugin_dir: &Path,
    ) -> String {
        format!(
            r#"#!/usr/bin/env bash
set -euo pipefail
input="$(cat)"
expected_cwd={expected_cwd}
expected_transcript_dir={expected_transcript_dir}
expected_plugin_dir={expected_plugin_dir}
cwd="$(printf '%s' "$input" | sed -n 's/.*"cwd":"\([^"]*\)".*/\1/p')"
transcript_path="$(printf '%s' "$input" | sed -n 's/.*"transcript_path":"\([^"]*\)".*/\1/p')"
if [ "$cwd" != "$expected_cwd" ]; then
  echo "unexpected cwd: $cwd" >&2
  exit 1
fi
case "$transcript_path" in
  "$expected_transcript_dir"/*) ;;
  *)
    echo "unexpected transcript path: $transcript_path" >&2
    exit 1
    ;;
esac
test -f "$transcript_path"
test "$CLAUDE_PLUGIN_ROOT" = "$expected_plugin_dir"
test "$CLAUDE_PLUGIN_DIR" = "$expected_plugin_dir"
test "$NENJO_PLUGIN_ROOT" = "$expected_plugin_dir"
printf '{{"status":"hook-ok"}}'
"#,
            expected_cwd = shell_quote(expected_cwd),
            expected_transcript_dir = shell_quote(expected_transcript_dir),
            expected_plugin_dir = shell_quote(expected_plugin_dir),
        )
    }

    fn stop_request_next_turn_hook_script(
        expected_cwd: &Path,
        expected_transcript_dir: &Path,
        expected_plugin_dir: &Path,
        accepted_marker: &str,
        prompt: &str,
        system_message: &str,
    ) -> String {
        format!(
            r#"#!/usr/bin/env bash
set -euo pipefail
input="$(cat)"
expected_cwd={expected_cwd}
expected_transcript_dir={expected_transcript_dir}
expected_plugin_dir={expected_plugin_dir}
accepted_marker={accepted_marker}
cwd="$(printf '%s' "$input" | sed -n 's/.*"cwd":"\([^"]*\)".*/\1/p')"
transcript_path="$(printf '%s' "$input" | sed -n 's/.*"transcript_path":"\([^"]*\)".*/\1/p')"
if [ "$cwd" != "$expected_cwd" ]; then
  echo "unexpected cwd: $cwd" >&2
  exit 1
fi
case "$transcript_path" in
  "$expected_transcript_dir"/*) ;;
  *)
    echo "unexpected transcript path: $transcript_path" >&2
    exit 1
    ;;
esac
test -f "$transcript_path"
test "$CLAUDE_PLUGIN_ROOT" = "$expected_plugin_dir"
test "$CLAUDE_PLUGIN_DIR" = "$expected_plugin_dir"
test "$NENJO_PLUGIN_ROOT" = "$expected_plugin_dir"
if grep -q "$accepted_marker" "$transcript_path"; then
  printf '{{"status":"hook-ok"}}'
else
  printf '{{"decision":"request_next_turn","prompt":{prompt},"systemMessage":{system_message}}}'
fi
"#,
            expected_cwd = shell_quote(expected_cwd),
            expected_transcript_dir = shell_quote(expected_transcript_dir),
            expected_plugin_dir = shell_quote(expected_plugin_dir),
            accepted_marker = shell_quote_str(accepted_marker),
            prompt = serde_json::json!(prompt),
            system_message = serde_json::json!(system_message),
        )
    }

    fn stop_always_request_next_turn_hook_script(
        expected_cwd: &Path,
        expected_transcript_dir: &Path,
        expected_plugin_dir: &Path,
        prompt: &str,
    ) -> String {
        format!(
            r#"#!/usr/bin/env bash
set -euo pipefail
input="$(cat)"
expected_cwd={expected_cwd}
expected_transcript_dir={expected_transcript_dir}
expected_plugin_dir={expected_plugin_dir}
cwd="$(printf '%s' "$input" | sed -n 's/.*"cwd":"\([^"]*\)".*/\1/p')"
transcript_path="$(printf '%s' "$input" | sed -n 's/.*"transcript_path":"\([^"]*\)".*/\1/p')"
if [ "$cwd" != "$expected_cwd" ]; then
  echo "unexpected cwd: $cwd" >&2
  exit 1
fi
case "$transcript_path" in
  "$expected_transcript_dir"/*) ;;
  *)
    echo "unexpected transcript path: $transcript_path" >&2
    exit 1
    ;;
esac
test -f "$transcript_path"
test "$CLAUDE_PLUGIN_ROOT" = "$expected_plugin_dir"
test "$CLAUDE_PLUGIN_DIR" = "$expected_plugin_dir"
test "$NENJO_PLUGIN_ROOT" = "$expected_plugin_dir"
printf '{{"decision":"request_next_turn","prompt":{prompt}}}'
"#,
            expected_cwd = shell_quote(expected_cwd),
            expected_transcript_dir = shell_quote(expected_transcript_dir),
            expected_plugin_dir = shell_quote(expected_plugin_dir),
            prompt = serde_json::json!(prompt),
        )
    }

    fn skill_stop_hook_script(
        expected_cwd: &Path,
        expected_transcript_dir: &Path,
        expected_plugin_dir: &Path,
        expected_skill_dir: &Path,
    ) -> String {
        format!(
            r#"#!/usr/bin/env bash
set -euo pipefail
input="$(cat)"
expected_cwd={expected_cwd}
expected_transcript_dir={expected_transcript_dir}
expected_plugin_dir={expected_plugin_dir}
expected_skill_dir={expected_skill_dir}
cwd="$(printf '%s' "$input" | sed -n 's/.*"cwd":"\([^"]*\)".*/\1/p')"
transcript_path="$(printf '%s' "$input" | sed -n 's/.*"transcript_path":"\([^"]*\)".*/\1/p')"
if [ "$cwd" != "$expected_cwd" ]; then
  echo "unexpected cwd: $cwd" >&2
  exit 1
fi
case "$transcript_path" in
  "$expected_transcript_dir"/*) ;;
  *)
    echo "unexpected transcript path: $transcript_path" >&2
    exit 1
    ;;
esac
test -f "$transcript_path"
test "$CLAUDE_PLUGIN_ROOT" = "$expected_plugin_dir"
test "$CLAUDE_PLUGIN_DIR" = "$expected_plugin_dir"
test "$NENJO_PLUGIN_ROOT" = "$expected_plugin_dir"
test "$CLAUDE_SKILL_DIR" = "$expected_skill_dir"
test "$NENJO_SKILL_DIR" = "$expected_skill_dir"
printf '{{"status":"hook-ok"}}'
"#,
            expected_cwd = shell_quote(expected_cwd),
            expected_transcript_dir = shell_quote(expected_transcript_dir),
            expected_plugin_dir = shell_quote(expected_plugin_dir),
            expected_skill_dir = shell_quote(expected_skill_dir),
        )
    }

    fn user_prompt_hook_script(
        expected_cwd: &Path,
        expected_transcript_dir: &Path,
        expected_plugin_dir: &Path,
        additional_context: &str,
    ) -> String {
        format!(
            r#"#!/usr/bin/env bash
set -euo pipefail
input="$(cat)"
expected_cwd={expected_cwd}
expected_transcript_dir={expected_transcript_dir}
expected_plugin_dir={expected_plugin_dir}
cwd="$(printf '%s' "$input" | sed -n 's/.*"cwd":"\([^"]*\)".*/\1/p')"
prompt="$(printf '%s' "$input" | sed -n 's/.*"prompt":"\([^"]*\)".*/\1/p')"
transcript_path="$(printf '%s' "$input" | sed -n 's/.*"transcript_path":"\([^"]*\)".*/\1/p')"
if [ "$cwd" != "$expected_cwd" ]; then
  echo "unexpected cwd: $cwd" >&2
  exit 1
fi
case "$transcript_path" in
  "$expected_transcript_dir"/*) ;;
  *)
    echo "unexpected transcript path: $transcript_path" >&2
    exit 1
    ;;
esac
case "$prompt" in
  *"Use the submitted task."*) ;;
  *)
    echo "unexpected prompt: $prompt" >&2
    exit 1
    ;;
esac
test -f "$transcript_path"
test "$CLAUDE_PLUGIN_ROOT" = "$expected_plugin_dir"
test "$CLAUDE_PLUGIN_DIR" = "$expected_plugin_dir"
test "$NENJO_PLUGIN_ROOT" = "$expected_plugin_dir"
printf '{{"status":"hook-ok","hookSpecificOutput":{{"additionalContext":{additional_context}}}}}'
"#,
            expected_cwd = shell_quote(expected_cwd),
            expected_transcript_dir = shell_quote(expected_transcript_dir),
            expected_plugin_dir = shell_quote(expected_plugin_dir),
            additional_context = serde_json::json!(additional_context),
        )
    }

    fn skill_user_prompt_hook_script(
        expected_cwd: &Path,
        expected_transcript_dir: &Path,
        expected_plugin_dir: &Path,
        expected_skill_dir: &Path,
        expected_prompt_fragment: &str,
        additional_context: &str,
    ) -> String {
        format!(
            r#"#!/usr/bin/env bash
set -euo pipefail
input="$(cat)"
expected_cwd={expected_cwd}
expected_transcript_dir={expected_transcript_dir}
expected_plugin_dir={expected_plugin_dir}
expected_skill_dir={expected_skill_dir}
expected_prompt_fragment={expected_prompt_fragment}
cwd="$(printf '%s' "$input" | sed -n 's/.*"cwd":"\([^"]*\)".*/\1/p')"
prompt="$(printf '%s' "$input" | sed -n 's/.*"prompt":"\([^"]*\)".*/\1/p')"
transcript_path="$(printf '%s' "$input" | sed -n 's/.*"transcript_path":"\([^"]*\)".*/\1/p')"
if [ "$cwd" != "$expected_cwd" ]; then
  echo "unexpected cwd: $cwd" >&2
  exit 1
fi
case "$transcript_path" in
  "$expected_transcript_dir"/*) ;;
  *)
    echo "unexpected transcript path: $transcript_path" >&2
    exit 1
    ;;
esac
case "$prompt" in
  *"$expected_prompt_fragment"*) ;;
  *)
    echo "unexpected prompt: $prompt" >&2
    exit 1
    ;;
esac
test -f "$transcript_path"
test "$CLAUDE_PLUGIN_ROOT" = "$expected_plugin_dir"
test "$CLAUDE_PLUGIN_DIR" = "$expected_plugin_dir"
test "$NENJO_PLUGIN_ROOT" = "$expected_plugin_dir"
test "$CLAUDE_SKILL_DIR" = "$expected_skill_dir"
test "$NENJO_SKILL_DIR" = "$expected_skill_dir"
printf '{{"status":"hook-ok","hookSpecificOutput":{{"additionalContext":{additional_context}}}}}'
"#,
            expected_cwd = shell_quote(expected_cwd),
            expected_transcript_dir = shell_quote(expected_transcript_dir),
            expected_plugin_dir = shell_quote(expected_plugin_dir),
            expected_skill_dir = shell_quote(expected_skill_dir),
            expected_prompt_fragment = shell_quote_str(expected_prompt_fragment),
            additional_context = serde_json::json!(additional_context),
        )
    }

    fn skill_tool_hook_script(
        expected_cwd: &Path,
        expected_transcript_dir: &Path,
        expected_plugin_dir: &Path,
        expected_skill_dir: &Path,
        expected_event: &str,
        expected_tool: &str,
    ) -> String {
        format!(
            r#"#!/usr/bin/env bash
set -euo pipefail
input="$(cat)"
expected_cwd={expected_cwd}
expected_transcript_dir={expected_transcript_dir}
expected_plugin_dir={expected_plugin_dir}
expected_skill_dir={expected_skill_dir}
expected_event={expected_event}
expected_tool={expected_tool}
cwd="$(printf '%s' "$input" | sed -n 's/.*"cwd":"\([^"]*\)".*/\1/p')"
transcript_path="$(printf '%s' "$input" | sed -n 's/.*"transcript_path":"\([^"]*\)".*/\1/p')"
event="$(printf '%s' "$input" | sed -n 's/.*"hook_event_name":"\([^"]*\)".*/\1/p')"
tool="$(printf '%s' "$input" | sed -n 's/.*"tool_name":"\([^"]*\)".*/\1/p')"
if [ "$cwd" != "$expected_cwd" ]; then
  echo "unexpected cwd: $cwd" >&2
  exit 1
fi
case "$transcript_path" in
  "$expected_transcript_dir"/*) ;;
  *)
    echo "unexpected transcript path: $transcript_path" >&2
    exit 1
    ;;
esac
test "$event" = "$expected_event"
test "$tool" = "$expected_tool"
test -f "$transcript_path"
test "$CLAUDE_PLUGIN_ROOT" = "$expected_plugin_dir"
test "$CLAUDE_PLUGIN_DIR" = "$expected_plugin_dir"
test "$NENJO_PLUGIN_ROOT" = "$expected_plugin_dir"
test "$CLAUDE_SKILL_DIR" = "$expected_skill_dir"
test "$NENJO_SKILL_DIR" = "$expected_skill_dir"
printf '{{"status":"hook-ok"}}'
"#,
            expected_cwd = shell_quote(expected_cwd),
            expected_transcript_dir = shell_quote(expected_transcript_dir),
            expected_plugin_dir = shell_quote(expected_plugin_dir),
            expected_skill_dir = shell_quote(expected_skill_dir),
            expected_event = shell_quote_str(expected_event),
            expected_tool = shell_quote_str(expected_tool),
        )
    }

    fn skill_pre_block_hook_script(
        expected_cwd: &Path,
        expected_transcript_dir: &Path,
        expected_plugin_dir: &Path,
        expected_skill_dir: &Path,
    ) -> String {
        format!(
            r#"#!/usr/bin/env bash
set -euo pipefail
input="$(cat)"
expected_cwd={expected_cwd}
expected_transcript_dir={expected_transcript_dir}
expected_plugin_dir={expected_plugin_dir}
expected_skill_dir={expected_skill_dir}
cwd="$(printf '%s' "$input" | sed -n 's/.*"cwd":"\([^"]*\)".*/\1/p')"
transcript_path="$(printf '%s' "$input" | sed -n 's/.*"transcript_path":"\([^"]*\)".*/\1/p')"
event="$(printf '%s' "$input" | sed -n 's/.*"hook_event_name":"\([^"]*\)".*/\1/p')"
tool="$(printf '%s' "$input" | sed -n 's/.*"tool_name":"\([^"]*\)".*/\1/p')"
if [ "$cwd" != "$expected_cwd" ]; then
  echo "unexpected cwd: $cwd" >&2
  exit 1
fi
case "$transcript_path" in
  "$expected_transcript_dir"/*) ;;
  *)
    echo "unexpected transcript path: $transcript_path" >&2
    exit 1
    ;;
esac
test "$event" = "PreToolUse"
test "$tool" = "write"
test -f "$transcript_path"
test "$CLAUDE_PLUGIN_ROOT" = "$expected_plugin_dir"
test "$CLAUDE_PLUGIN_DIR" = "$expected_plugin_dir"
test "$NENJO_PLUGIN_ROOT" = "$expected_plugin_dir"
test "$CLAUDE_SKILL_DIR" = "$expected_skill_dir"
test "$NENJO_SKILL_DIR" = "$expected_skill_dir"
printf '{{"decision":"block","reason":"no writes","systemMessage":"write blocked"}}'
"#,
            expected_cwd = shell_quote(expected_cwd),
            expected_transcript_dir = shell_quote(expected_transcript_dir),
            expected_plugin_dir = shell_quote(expected_plugin_dir),
            expected_skill_dir = shell_quote(expected_skill_dir),
        )
    }

    fn skill_post_response_hook_script(
        expected_cwd: &Path,
        expected_transcript_dir: &Path,
        expected_plugin_dir: &Path,
        expected_skill_dir: &Path,
        expected_tool: &str,
        expected_success: bool,
    ) -> String {
        let expected_success = if expected_success { "true" } else { "false" };
        format!(
            r#"#!/usr/bin/env bash
set -euo pipefail
input="$(cat)"
expected_cwd={expected_cwd}
expected_transcript_dir={expected_transcript_dir}
expected_plugin_dir={expected_plugin_dir}
expected_skill_dir={expected_skill_dir}
expected_tool={expected_tool}
expected_success={expected_success}
cwd="$(printf '%s' "$input" | sed -n 's/.*"cwd":"\([^"]*\)".*/\1/p')"
transcript_path="$(printf '%s' "$input" | sed -n 's/.*"transcript_path":"\([^"]*\)".*/\1/p')"
event="$(printf '%s' "$input" | sed -n 's/.*"hook_event_name":"\([^"]*\)".*/\1/p')"
tool="$(printf '%s' "$input" | sed -n 's/.*"tool_name":"\([^"]*\)".*/\1/p')"
if [ "$cwd" != "$expected_cwd" ]; then
  echo "unexpected cwd: $cwd" >&2
  exit 1
fi
case "$transcript_path" in
  "$expected_transcript_dir"/*) ;;
  *)
    echo "unexpected transcript path: $transcript_path" >&2
    exit 1
    ;;
esac
test "$event" = "PostToolUse"
test "$tool" = "$expected_tool"
test -f "$transcript_path"
test "$CLAUDE_PLUGIN_ROOT" = "$expected_plugin_dir"
test "$CLAUDE_PLUGIN_DIR" = "$expected_plugin_dir"
test "$NENJO_PLUGIN_ROOT" = "$expected_plugin_dir"
test "$CLAUDE_SKILL_DIR" = "$expected_skill_dir"
test "$NENJO_SKILL_DIR" = "$expected_skill_dir"
printf '%s' "$input" | grep -q '"tool_response":'
if [ "$expected_success" = "true" ]; then
  printf '%s' "$input" | grep -q '"success":true'
  printf '%s' "$input" | grep -q '"error":null'
  printf '%s' "$input" | grep -q '"output":"Written '
else
  printf '%s' "$input" | grep -q '"success":false'
  printf '%s' "$input" | grep -q '"error":"Failed to resolve file path:'
  printf '%s' "$input" | grep -q '"output":""'
fi
printf '{{"status":"hook-ok"}}'
"#,
            expected_cwd = shell_quote(expected_cwd),
            expected_transcript_dir = shell_quote(expected_transcript_dir),
            expected_plugin_dir = shell_quote(expected_plugin_dir),
            expected_skill_dir = shell_quote(expected_skill_dir),
            expected_tool = shell_quote_str(expected_tool),
            expected_success = shell_quote_str(expected_success),
        )
    }

    fn skill_mcp_fixture_script() -> String {
        r#"#!/usr/bin/env bash
set -euo pipefail
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-03-26","capabilities":{},"serverInfo":{"name":"fixture","version":"0.1.0"}}}'
      ;;
    *'"method":"notifications/initialized"'*)
      ;;
    *'"method":"tools/list"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"tools":[{"name":"review","description":"Review a topic with the active skill MCP server","inputSchema":{"type":"object","properties":{"topic":{"type":"string"}},"required":["topic"]}}]}}'
      ;;
    *'"method":"tools/call"'*)
      topic="$(printf '%s' "$line" | sed -n 's/.*"topic":"\([^"]*\)".*/\1/p')"
      printf '{"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":"skill-mcp-review-ok:%s"}]}}\n' "$topic"
      ;;
    *)
      printf '%s\n' '{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"unknown method"}}'
      ;;
  esac
done
"#
        .to_string()
    }

    fn shell_quote(path: &Path) -> String {
        let value = path.display().to_string();
        format!("'{}'", value.replace('\'', r#"'"'"'"#))
    }

    fn shell_quote_str(value: &str) -> String {
        format!("'{}'", value.replace('\'', r#"'"'"'"#))
    }

    #[derive(Clone, Copy)]
    enum HookStreamKind {
        Started,
        Completed,
    }

    fn count_hook_events(
        responses: &[Response],
        kind: HookStreamKind,
        expected_event: &str,
        expected_source: &str,
    ) -> usize {
        responses
            .iter()
            .filter(|response| match response {
                Response::AgentResponse { payload, .. } => match (kind, payload) {
                    (
                        HookStreamKind::Started,
                        StreamEvent::HookStarted {
                            hook,
                            hook_event,
                            source,
                            ..
                        },
                    )
                    | (
                        HookStreamKind::Completed,
                        StreamEvent::HookCompleted {
                            hook,
                            hook_event,
                            source,
                            ..
                        },
                    ) => {
                        hook == "Ralph Loop Stop"
                            && hook_event == expected_event
                            && source == expected_source
                    }
                    _ => false,
                },
                _ => false,
            })
            .count()
    }

    fn count_successful_hook_completions(
        responses: &[Response],
        expected_event: &str,
        expected_source: &str,
    ) -> usize {
        responses
            .iter()
            .filter(|response| {
                let Response::AgentResponse { payload, .. } = response else {
                    return false;
                };
                let StreamEvent::HookCompleted {
                    hook,
                    hook_event,
                    source,
                    success,
                    blocked,
                    payload,
                    ..
                } = payload
                else {
                    return false;
                };
                hook == "Ralph Loop Stop"
                    && hook_event == expected_event
                    && source == expected_source
                    && *success
                    && !blocked
                    && payload
                        .as_ref()
                        .and_then(|payload| payload.get("output_preview"))
                        .and_then(Value::as_str)
                        .is_some_and(|preview| preview.contains("hook-ok"))
            })
            .count()
    }

    fn hook_completed_blocked(
        responses: &[Response],
        expected_event: &str,
        expected_source: &str,
        expected_reason: &str,
    ) -> bool {
        responses.iter().any(|response| {
            let Response::AgentResponse { payload, .. } = response else {
                return false;
            };
            let StreamEvent::HookCompleted {
                hook,
                hook_event,
                source,
                blocked,
                payload,
                ..
            } = payload
            else {
                return false;
            };
            hook == "Ralph Loop Stop"
                && hook_event == expected_event
                && source == expected_source
                && *blocked
                && payload
                    .as_ref()
                    .and_then(|payload| payload.get("reason"))
                    .and_then(Value::as_str)
                    .is_some_and(|reason| reason.contains(expected_reason))
        })
    }

    fn done_output_contains(responses: &[Response], expected_output: &str) -> bool {
        responses.iter().any(|response| {
            let Response::AgentResponse { payload, .. } = response else {
                return false;
            };
            let StreamEvent::Done { payload, .. } = payload else {
                return false;
            };
            payload
                .as_ref()
                .and_then(Value::as_str)
                .is_some_and(|output| output.contains(expected_output))
        })
    }

    fn hook_completed_successfully(
        responses: &[Response],
        expected_event: &str,
        expected_source: &str,
    ) -> bool {
        responses.iter().any(|response| {
            let Response::AgentResponse { payload, .. } = response else {
                return false;
            };
            let StreamEvent::HookCompleted {
                hook,
                hook_event,
                source,
                success,
                blocked,
                payload,
                ..
            } = payload
            else {
                return false;
            };
            hook == "Ralph Loop Stop"
                && hook_event == expected_event
                && source == expected_source
                && *success
                && !blocked
                && payload
                    .as_ref()
                    .and_then(|payload| payload.get("output_preview"))
                    .and_then(Value::as_str)
                    .is_some_and(|preview| preview.contains("hook-ok"))
        })
    }
}
