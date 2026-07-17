//! Responses sent from the harness to the backend (`responses`).

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{Capability, EncryptedPayload};

/// Agent identity attached to step events so the frontend can render identicons.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepAgent {
    pub agent: String,
    pub agent_name: Option<String>,
    pub agent_color: Option<String>,
}

/// Routine step scope attached to execution trace events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionTraceRoutineStep {
    pub step_slug: String,
    pub step_run_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_type: Option<String>,
}

/// A workflow/routine progress event inside an execution run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionWorkflowStepEvent {
    /// One of: `step_started`, `step_completed`, `step_failed`,
    /// `step_warning`, `progress`, or worktree lifecycle event names.
    pub event_type: String,
    pub step_name: String,
    pub step_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default)]
    pub data: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encrypted_payload: Option<EncryptedPayload>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<StepAgent>,
}

/// Durable artifacts and usage produced by a task execution.
///
/// Lifecycle outcome is intentionally excluded. [`TaskExecutionState`] is the
/// sole authority for completion, failure, rejection, and cancellation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionTaskArtifactsEvent {
    #[serde(default)]
    pub total_input_tokens: u64,
    #[serde(default)]
    pub total_output_tokens: u64,
    /// Encrypted durable outputs finalized atomically with this terminal event.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<TaskAttachmentManifest>,
}

/// Stable identifier for a durable task attachment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TaskAttachmentId(Uuid);

impl TaskAttachmentId {
    pub const fn new(id: Uuid) -> Self {
        Self(id)
    }

    pub const fn into_uuid(self) -> Uuid {
        self.0
    }
}

/// Closed set of task attachment kinds supported by the initial protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskAttachmentKind {
    FinalOutput,
    RoutineHandoff,
}

/// Provenance for one activated edge reaching a terminal routine step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutineHandoffSource {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routine_id: Option<Uuid>,
    pub source_step_slug: String,
    pub destination_step_slug: String,
    pub edge_condition: String,
}

/// Opaque encrypted attachment delivered with a terminal task event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskAttachmentManifest {
    pub id: TaskAttachmentId,
    pub kind: TaskAttachmentKind,
    pub name: String,
    pub content_type: String,
    pub byte_size: u64,
    pub encrypted_payload: EncryptedPayload,
    pub content_digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<RoutineHandoffSource>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceTranscriptSegment {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_seconds: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_seconds: Option<f64>,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// Stable discriminator for canonical execution-scoped event payloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ExecutionEventKind {
    #[serde(rename = "workflow.step")]
    WorkflowStep,
    #[serde(rename = "agent.trace")]
    AgentTrace,
    #[serde(rename = "task.artifacts")]
    TaskArtifacts,
}

impl ExecutionEventKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::WorkflowStep => "workflow.step",
            Self::AgentTrace => "agent.trace",
            Self::TaskArtifacts => "task.artifacts",
        }
    }
}

impl std::fmt::Display for ExecutionEventKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Canonical execution-scoped event payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data")]
pub enum ExecutionEventPayload {
    /// Coarse workflow/routine progress used for task and step status.
    #[serde(rename = "workflow.step")]
    WorkflowStep(ExecutionWorkflowStepEvent),
    /// Fine-grained agent trace event shared with chat.
    #[serde(rename = "agent.trace")]
    AgentTrace {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        routine_step: Option<ExecutionTraceRoutineStep>,
        payload: StreamEvent,
    },
    /// Outputs and usage produced by a terminal task execution.
    #[serde(rename = "task.artifacts")]
    TaskArtifacts(ExecutionTaskArtifactsEvent),
}

impl ExecutionEventPayload {
    pub const fn kind(&self) -> ExecutionEventKind {
        match self {
            Self::WorkflowStep(_) => ExecutionEventKind::WorkflowStep,
            Self::AgentTrace { .. } => ExecutionEventKind::AgentTrace,
            Self::TaskArtifacts(_) => ExecutionEventKind::TaskArtifacts,
        }
    }
}

// ---------------------------------------------------------------------------
// Top-level response wrapper
// ---------------------------------------------------------------------------

/// Lifecycle state maintained by the worker's durable task inbox.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum TaskExecutionState {
    Queued,
    Running,
    Completed,
    Failed { error: String },
    Cancelled,
    Rejected { reason: String },
}

/// Trigger accepted by an immediate platform-to-worker task command.
///
/// Scheduled work is materialized by the worker and therefore appears only in
/// [`TaskExecutionOrigin`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskExecutionTrigger {
    Manual,
    Retry,
}

/// Immutable origin of a durable worker-inbox execution receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TaskExecutionOrigin {
    Manual,
    Retry,
    Schedule {
        schedule_id: Uuid,
        scheduled_for: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        next_run_at: Option<String>,
        /// Revision of the exact cached assignment used for materialization.
        assignment_revision: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project: Option<String>,
        target: crate::TaskExecutionTarget,
    },
}

/// A response sent from the harness back to the backend.
///
/// Discriminated by the `type` field in JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Response {
    /// Wraps a [`StreamEvent`] for real-time streaming to the frontend.
    #[serde(rename = "agent_response")]
    AgentResponse {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<Uuid>,
        payload: StreamEvent,
    },

    /// Canonical execution-scoped event.
    #[serde(rename = "execution.event")]
    ExecutionEvent {
        execution_run_id: String,
        #[serde(default)]
        task_id: Option<String>,
        event: ExecutionEventPayload,
    },

    /// Durable worker-inbox state for a task execution.
    #[serde(rename = "task.execution_state")]
    TaskExecutionState {
        execution_run_id: Uuid,
        task_id: Uuid,
        state: TaskExecutionState,
        origin: TaskExecutionOrigin,
        /// Monotonic revision of the worker's durable inbox receipt.
        #[serde(default)]
        revision: u64,
        /// Distinguishes a genuine worker restart recovery from a stale queued replay.
        #[serde(default)]
        recovered: bool,
    },

    /// Repo sync completed (or failed) for a project.
    #[serde(rename = "repo.sync_complete")]
    RepoSyncComplete {
        project: String,
        success: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },

    /// Org-scoped encrypted push notification.
    #[serde(rename = "push.notification")]
    PushNotification {
        agent: String,
        session_id: Uuid,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        recipient_user_id: Option<Uuid>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        recipient_handle: Option<String>,
        encrypted_payload: EncryptedPayload,
    },

    /// Completed push-to-talk transcription. The transcript text is encrypted
    /// for the initiating user; the platform stores and routes ciphertext.
    #[serde(rename = "voice_input.transcribed")]
    VoiceInputTranscribed {
        job_id: Uuid,
        session_id: Uuid,
        encrypted_transcript: EncryptedPayload,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        language: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_seconds: Option<f64>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        segments: Vec<VoiceTranscriptSegment>,
        provider: String,
        model: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        metadata: Option<serde_json::Value>,
    },

    /// Failed push-to-talk transcription.
    #[serde(rename = "voice_input.failed")]
    VoiceInputFailed {
        job_id: Uuid,
        session_id: Uuid,
        error_code: String,
        error_message: String,
    },

    /// Confirms receipt of a command (sent after processing begins).
    #[serde(rename = "delivery_receipt")]
    DeliveryReceipt { message_id: String },

    /// Worker presence heartbeat — sent on startup and periodically.
    /// The backend uses this to set the Redis presence key.
    #[serde(rename = "worker.heartbeat")]
    WorkerHeartbeat {
        /// Unique instance ID for this worker process (generated at startup).
        worker_id: Uuid,
        /// Capabilities this worker handles.
        capabilities: Vec<Capability>,
        /// Application version (e.g. "0.1.0"). Used by the backend for
        /// backward compatibility decisions.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version: Option<String>,
    },

    /// Response to a `worker.ping` command — proves the worker is alive.
    #[serde(rename = "worker.pong")]
    WorkerPong,

    /// Sent once on initial connection to register the worker with the backend.
    #[serde(rename = "worker.registered")]
    WorkerRegistered {
        /// Unique instance ID for this worker process.
        worker_id: Uuid,
        /// Capabilities this worker handles.
        capabilities: Vec<Capability>,
        /// Application version (e.g. "0.1.0"). Used by the backend for
        /// backward compatibility decisions.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version: Option<String>,
    },
}

impl std::fmt::Display for Response {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AgentResponse {
                session_id,
                payload,
            } => match session_id {
                Some(session_id) => write!(f, "agent_response(session={session_id}, {payload})"),
                None => write!(f, "agent_response({payload})"),
            },
            Self::ExecutionEvent {
                execution_run_id,
                event,
                ..
            } => match event {
                ExecutionEventPayload::WorkflowStep(step) => write!(
                    f,
                    "execution.event(run={execution_run_id}, {}={}, step={})",
                    event.kind(),
                    step.event_type,
                    step.step_name
                ),
                ExecutionEventPayload::AgentTrace {
                    routine_step,
                    payload,
                } => {
                    if let Some(routine_step) = routine_step {
                        write!(
                            f,
                            "execution.event(run={execution_run_id}, {}, step={}, {payload})",
                            event.kind(),
                            routine_step.step_slug
                        )
                    } else {
                        write!(
                            f,
                            "execution.event(run={execution_run_id}, {}, {payload})",
                            event.kind()
                        )
                    }
                }
                ExecutionEventPayload::TaskArtifacts(artifacts) => write!(
                    f,
                    "execution.event(run={execution_run_id}, {}, attachments={})",
                    event.kind(),
                    artifacts.attachments.len()
                ),
            },
            Self::TaskExecutionState {
                execution_run_id,
                state,
                origin,
                ..
            } => write!(
                f,
                "task.execution_state(run={execution_run_id}, state={state:?}, origin={origin:?})"
            ),
            Self::RepoSyncComplete {
                project, success, ..
            } => {
                write!(
                    f,
                    "repo.sync_complete(project={project}, success={success})"
                )
            }
            Self::PushNotification {
                agent, session_id, ..
            } => {
                write!(f, "push.notification(agent={agent}, session={session_id})")
            }
            Self::VoiceInputTranscribed {
                job_id, session_id, ..
            } => {
                write!(
                    f,
                    "voice_input.transcribed(job={job_id}, session={session_id})"
                )
            }
            Self::VoiceInputFailed {
                job_id,
                session_id,
                error_code,
                ..
            } => {
                write!(
                    f,
                    "voice_input.failed(job={job_id}, session={session_id}, code={error_code})"
                )
            }
            Self::DeliveryReceipt { message_id } => write!(f, "delivery_receipt({message_id})"),
            Self::WorkerPong => write!(f, "worker.pong"),
            Self::WorkerHeartbeat { worker_id, .. } => {
                write!(f, "worker.heartbeat(worker={worker_id})")
            }
            Self::WorkerRegistered { worker_id, .. } => {
                write!(f, "worker.registered(worker={worker_id})")
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Stream events (real-time agent execution)
// ---------------------------------------------------------------------------

/// Events streamed during agent execution and bridged to clients by the platform.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event_type", content = "data")]
pub enum StreamEvent {
    /// A chat/execution run started.
    RunStarted {
        run_id: String,
        session_id: String,
        /// Durable user-message identity that initiated this chat run.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input_message_id: Option<Uuid>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_run_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_name: Option<String>,
    },

    /// A chat/execution run completed.
    RunCompleted { run_id: String, session_id: String },

    /// A chat/execution run failed.
    RunFailed {
        run_id: String,
        session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        payload: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        encrypted_payload: Option<EncryptedPayload>,
    },

    /// A chat/execution run was cancelled.
    RunCancelled { run_id: String, session_id: String },

    /// A model provider request started.
    ModelRequestStarted {
        run_id: String,
        request_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_call_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
    },

    /// Assistant prose delta from the model provider.
    AssistantTextDelta {
        run_id: String,
        request_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        payload: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        encrypted_payload: Option<EncryptedPayload>,
    },

    /// User-visible assistant response emitted by the response tool.
    AssistantResponse {
        run_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        payload: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        encrypted_payload: Option<EncryptedPayload>,
    },

    /// A model provider request completed.
    ModelRequestCompleted {
        run_id: String,
        request_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_call_id: Option<String>,
    },

    /// A single tool invocation started.
    ToolCallStarted {
        run_id: String,
        batch_id: String,
        call_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_call_id: Option<String>,
        tool_name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        payload: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        encrypted_payload: Option<EncryptedPayload>,
    },

    /// Incremental output from a tool invocation.
    ToolOutputDelta {
        run_id: String,
        call_id: String,
        stream: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        payload: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        encrypted_payload: Option<EncryptedPayload>,
    },

    /// A single tool invocation completed.
    ToolCallCompleted {
        run_id: String,
        batch_id: String,
        call_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_call_id: Option<String>,
        success: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        payload: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        encrypted_payload: Option<EncryptedPayload>,
    },

    /// An active hook started executing.
    HookStarted {
        agent: String,
        hook: String,
        hook_event: String,
        hook_type: String,
        source: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        payload: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        encrypted_payload: Option<EncryptedPayload>,
    },

    /// An active hook completed executing.
    HookCompleted {
        agent: String,
        hook: String,
        hook_event: String,
        hook_type: String,
        source: String,
        success: bool,
        blocked: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        payload: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        encrypted_payload: Option<EncryptedPayload>,
    },

    /// Canonical lifecycle or signal event for a long-running async operation.
    AsyncOperationEvent {
        operation_id: String,
        kind: String,
        label: String,
        status: String,
        signal: String,
        model_visible: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_operation_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_tool_name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        payload: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        encrypted_payload: Option<EncryptedPayload>,
    },

    /// Canonical transcript event for a long-running async operation.
    AsyncOperationTranscript {
        operation_id: String,
        kind: String,
        label: String,
        event: AsyncOperationTranscriptEvent,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        payload: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        encrypted_payload: Option<EncryptedPayload>,
    },

    /// An error occurred during execution.
    Error {
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        payload: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        encrypted_payload: Option<EncryptedPayload>,
    },

    /// Execution completed successfully.
    Done {
        /// Run producing this terminal response.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<String>,
        /// Durable user-message identity this response answers.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input_message_id: Option<Uuid>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        payload: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        encrypted_payload: Option<EncryptedPayload>,
        #[serde(default)]
        total_input_tokens: u64,
        #[serde(default)]
        total_output_tokens: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<Uuid>,
    },

    /// A domain session was entered.
    DomainEntered {
        session_id: Uuid,
        domain_name: String,
    },

    /// A domain session was exited.
    DomainExited {
        session_id: Uuid,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        artifact_id: Option<Uuid>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        document_id: Option<Uuid>,
    },

    /// Chat history was compacted via LLM summarization.
    MessageCompacted {
        messages_before: usize,
        messages_after: usize,
    },

    /// Execution was paused (agent will stop before the next LLM call).
    Paused,

    /// Execution was resumed after a pause.
    Resumed,
}

impl std::fmt::Display for StreamEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RunStarted {
                run_id, session_id, ..
            } => write!(f, "run_started(run={run_id}, session={session_id})"),
            Self::RunCompleted { run_id, .. } => write!(f, "run_completed(run={run_id})"),
            Self::RunFailed { run_id, .. } => write!(f, "run_failed(run={run_id})"),
            Self::RunCancelled { run_id, .. } => write!(f, "run_cancelled(run={run_id})"),
            Self::ModelRequestStarted {
                run_id,
                request_id,
                model,
                ..
            } => write!(
                f,
                "model_request_started(run={run_id}, request={request_id}, model={})",
                model.as_deref().unwrap_or("-")
            ),
            Self::AssistantTextDelta {
                run_id,
                request_id,
                payload,
                encrypted_payload,
            } => write!(
                f,
                "assistant_text_delta(run={run_id}, request={request_id}, payload={}, encrypted={})",
                payload.is_some(),
                encrypted_payload.is_some()
            ),
            Self::AssistantResponse {
                run_id,
                payload,
                encrypted_payload,
            } => write!(
                f,
                "assistant_response(run={run_id}, payload={}, encrypted={})",
                payload.is_some(),
                encrypted_payload.is_some()
            ),
            Self::ModelRequestCompleted {
                run_id, request_id, ..
            } => {
                write!(
                    f,
                    "model_request_completed(run={run_id}, request={request_id})"
                )
            }
            Self::ToolCallStarted {
                run_id,
                batch_id,
                call_id,
                tool_name,
                ..
            } => write!(
                f,
                "tool_call_started(run={run_id}, batch={batch_id}, call={call_id}, tool={tool_name})"
            ),
            Self::ToolOutputDelta {
                run_id,
                call_id,
                stream,
                payload,
                encrypted_payload,
            } => write!(
                f,
                "tool_output_delta(run={run_id}, call={call_id}, stream={stream}, payload={}, encrypted={})",
                payload.is_some(),
                encrypted_payload.is_some()
            ),
            Self::ToolCallCompleted {
                run_id,
                batch_id,
                call_id,
                parent_call_id,
                success,
                ..
            } => write!(
                f,
                "tool_call_completed(run={run_id}, batch={batch_id}, call={call_id}, parent={}, success={success})",
                parent_call_id.as_deref().unwrap_or("-")
            ),
            Self::HookStarted {
                agent,
                hook,
                hook_event,
                source,
                ..
            } => write!(
                f,
                "hook_started({hook}, event={hook_event}, source={source}, agent={agent})"
            ),
            Self::HookCompleted {
                agent,
                hook,
                hook_event,
                source,
                success,
                blocked,
                ..
            } => write!(
                f,
                "hook_completed({hook}, event={hook_event}, source={source}, agent={agent}, success={success}, blocked={blocked})"
            ),
            Self::AsyncOperationEvent {
                operation_id,
                kind,
                signal,
                status,
                ..
            } => write!(
                f,
                "async_operation_event({signal}, id={operation_id}, kind={kind}, status={status})"
            ),
            Self::AsyncOperationTranscript {
                operation_id,
                kind,
                event,
                ..
            } => write!(
                f,
                "async_operation_transcript(id={operation_id}, kind={kind}, event={})",
                event.kind
            ),
            Self::Error { message, .. } => write!(f, "error({message})"),
            Self::Done {
                payload,
                encrypted_payload,
                ..
            } => write!(
                f,
                "done(payload={}, encrypted={})",
                if payload.is_some() { "yes" } else { "no" },
                if encrypted_payload.is_some() {
                    "yes"
                } else {
                    "no"
                }
            ),
            Self::DomainEntered {
                session_id,
                domain_name,
            } => write!(f, "domain_entered({domain_name}, session={session_id})"),
            Self::DomainExited { session_id, .. } => {
                write!(f, "domain_exited(session={session_id})")
            }
            Self::MessageCompacted {
                messages_before,
                messages_after,
            } => write!(f, "message_compacted({messages_before}->{messages_after})"),
            Self::Paused => write!(f, "paused"),
            Self::Resumed => write!(f, "resumed"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AsyncOperationTranscriptEvent {
    pub kind: String,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub success: Option<bool>,
}

// ---------------------------------------------------------------------------
// Supporting types
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Builder helpers
// ---------------------------------------------------------------------------

impl Response {
    /// Build a canonical workflow step execution event.
    pub fn workflow_step_event(
        execution_run_id: Uuid,
        task_id: Option<Uuid>,
        event_type: impl Into<String>,
        step_name: impl Into<String>,
        step_type: impl Into<String>,
        duration_ms: Option<u64>,
        data: serde_json::Value,
    ) -> Self {
        Self::ExecutionEvent {
            execution_run_id: execution_run_id.to_string(),
            task_id: task_id.map(|id| id.to_string()),
            event: ExecutionEventPayload::WorkflowStep(ExecutionWorkflowStepEvent {
                event_type: event_type.into(),
                step_name: step_name.into(),
                step_type: step_type.into(),
                duration_ms,
                data,
                payload: None,
                encrypted_payload: None,
                agent: None,
            }),
        }
    }

    /// Build a canonical task artifact execution event.
    pub fn task_artifacts(
        execution_run_id: Uuid,
        task_id: Option<Uuid>,
        total_input_tokens: u64,
        total_output_tokens: u64,
    ) -> Self {
        Self::ExecutionEvent {
            execution_run_id: execution_run_id.to_string(),
            task_id: task_id.map(|id| id.to_string()),
            event: ExecutionEventPayload::TaskArtifacts(ExecutionTaskArtifactsEvent {
                total_input_tokens,
                total_output_tokens,
                attachments: Vec::new(),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Command, EncryptedPayload, Envelope};

    #[test]
    fn final_output_attachment_roundtrips_with_size() {
        let org_id = Uuid::new_v4();
        let attachment_id = Uuid::new_v4();
        let attachment = TaskAttachmentManifest {
            id: TaskAttachmentId::new(attachment_id),
            kind: TaskAttachmentKind::FinalOutput,
            name: "Final output".to_string(),
            content_type: "text/markdown".to_string(),
            byte_size: 5,
            encrypted_payload: EncryptedPayload {
                account_id: org_id,
                encryption_scope: Some("org".to_string()),
                object_id: attachment_id,
                object_type: "task.attachment".to_string(),
                algorithm: "aes-256-gcm".to_string(),
                key_version: 1,
                nonce: "nonce".to_string(),
                ciphertext: "ciphertext".to_string(),
            },
            content_digest: "sha256:test".to_string(),
            source: None,
        };
        let json = serde_json::to_value(&attachment).expect("serialize attachment");
        assert_eq!(json["kind"], "final_output");
        assert_eq!(json["byte_size"], 5);
        let decoded: TaskAttachmentManifest =
            serde_json::from_value(json).expect("deserialize attachment");
        assert_eq!(decoded.kind, TaskAttachmentKind::FinalOutput);
        assert_eq!(decoded.byte_size, 5);
        assert!(decoded.source.is_none());
    }

    #[test]
    fn command_chat_message_roundtrip() {
        let cmd = Command::ChatMessage {
            id: Some("msg-123".into()),
            content: "hello".into(),
            encrypted_content: None,
            hidden: true,
            project: None,
            routine: None,
            agent: Some("demo_agent".into()),
            target_type: None,
            target: None,
            domain_session_id: None,
            domain_activation: None,
            session_id: Uuid::nil(),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains(r#""type":"chat.message""#));
        assert!(json.contains(r#""hidden":true"#));
        let parsed: Command = serde_json::from_str(&json).unwrap();
        match parsed {
            Command::ChatMessage {
                content, hidden, ..
            } => {
                assert_eq!(content, "hello");
                assert!(hidden);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn command_chat_message_with_encrypted_content_roundtrip() {
        let payload = EncryptedPayload {
            account_id: Uuid::nil(),
            encryption_scope: None,
            object_id: Uuid::new_v4(),
            object_type: "agent_prompt".into(),
            algorithm: "aes-256-gcm".into(),
            key_version: 1,
            nonce: "bm9uY2U=".into(),
            ciphertext: "Y2lwaGVydGV4dA==".into(),
        };
        let cmd = Command::ChatMessage {
            id: None,
            content: String::new(),
            encrypted_content: Some(payload.clone()),
            hidden: false,
            project: None,
            routine: None,
            agent: None,
            target_type: None,
            target: None,
            domain_session_id: None,
            domain_activation: None,
            session_id: Uuid::nil(),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains(r#""encrypted_content""#));

        let parsed: Command = serde_json::from_str(&json).unwrap();
        match parsed {
            Command::ChatMessage {
                encrypted_content, ..
            } => {
                let parsed_payload = encrypted_content.expect("encrypted content should exist");
                assert_eq!(parsed_payload.account_id, payload.account_id);
                assert_eq!(parsed_payload.object_id, payload.object_id);
                assert_eq!(parsed_payload.object_type, payload.object_type);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn response_workflow_step_event_builder() {
        let resp = Response::workflow_step_event(
            Uuid::nil(),
            Some(Uuid::nil()),
            "step_started",
            "Implementation",
            "agent",
            None,
            serde_json::json!({}),
        );
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains(r#""type":"execution.event""#));
        assert!(json.contains(r#""kind":"workflow.step""#));
        assert!(json.contains(r#""event_type":"step_started""#));
    }

    #[test]
    fn response_task_artifacts_builder() {
        let resp = Response::task_artifacts(Uuid::nil(), None, 100, 50);
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains(r#""type":"execution.event""#));
        assert!(json.contains(r#""kind":"task.artifacts""#));
        assert!(!json.contains(r#""success""#));
    }

    #[test]
    fn execution_event_kind_uses_stable_wire_values() {
        assert_eq!(ExecutionEventKind::WorkflowStep.as_str(), "workflow.step");
        assert_eq!(ExecutionEventKind::AgentTrace.as_str(), "agent.trace");
        assert_eq!(ExecutionEventKind::TaskArtifacts.as_str(), "task.artifacts");
        assert_eq!(
            serde_json::to_string(&ExecutionEventKind::WorkflowStep).unwrap(),
            r#""workflow.step""#,
        );
    }

    #[test]
    fn execution_event_payload_exposes_typed_kind() {
        let event = ExecutionEventPayload::TaskArtifacts(ExecutionTaskArtifactsEvent {
            total_input_tokens: 1,
            total_output_tokens: 2,
            attachments: Vec::new(),
        });

        assert_eq!(event.kind(), ExecutionEventKind::TaskArtifacts);
    }

    #[test]
    fn execution_event_wire_shape_is_canonical_for_all_kinds() {
        let run_id = Uuid::nil();
        let task_id = Uuid::nil();
        let workflow = Response::workflow_step_event(
            run_id,
            Some(task_id),
            "step_started",
            "plan",
            "agent",
            Some(12),
            serde_json::json!({"step_slug": "plan"}),
        );
        let agent_trace = Response::ExecutionEvent {
            execution_run_id: run_id.to_string(),
            task_id: Some(task_id.to_string()),
            event: ExecutionEventPayload::AgentTrace {
                routine_step: Some(ExecutionTraceRoutineStep {
                    step_slug: "plan".to_string(),
                    step_run_id: task_id,
                    step_name: Some("plan".to_string()),
                    step_type: Some("agent".to_string()),
                }),
                payload: StreamEvent::AssistantTextDelta {
                    run_id: "trace-run".to_string(),
                    request_id: "request-1".to_string(),
                    payload: Some(serde_json::json!({"text_preview": "Planning"})),
                    encrypted_payload: None,
                },
            },
        };
        let artifacts = Response::task_artifacts(run_id, Some(task_id), 1, 2);

        let workflow_json = serde_json::to_value(&workflow).unwrap();
        assert_eq!(workflow_json["type"], "execution.event");
        assert_eq!(workflow_json["event"]["kind"], "workflow.step");
        assert_eq!(workflow_json["event"]["data"]["step_name"], "plan");

        let trace_json = serde_json::to_value(&agent_trace).unwrap();
        assert_eq!(trace_json["type"], "execution.event");
        assert_eq!(trace_json["event"]["kind"], "agent.trace");
        assert_eq!(
            trace_json["event"]["data"]["routine_step"]["step_slug"],
            "plan"
        );
        assert_eq!(
            trace_json["event"]["data"]["payload"]["event_type"],
            "AssistantTextDelta"
        );

        let artifacts_json = serde_json::to_value(&artifacts).unwrap();
        assert_eq!(artifacts_json["type"], "execution.event");
        assert_eq!(artifacts_json["event"]["kind"], "task.artifacts");
        assert_eq!(artifacts_json["event"]["data"]["total_input_tokens"], 1);
    }

    #[test]
    fn response_agent_response_roundtrip() {
        let resp = Response::AgentResponse {
            session_id: Some(Uuid::nil()),
            payload: StreamEvent::Done {
                run_id: Some("run-1".into()),
                input_message_id: Some(Uuid::nil()),
                payload: Some(serde_json::Value::String("result".into())),
                encrypted_payload: None,
                total_input_tokens: 0,
                total_output_tokens: 0,
                project: None,
                agent: None,
                session_id: None,
            },
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains(r#""type":"agent_response""#));
        let parsed: Response = serde_json::from_str(&json).unwrap();
        match parsed {
            Response::AgentResponse {
                session_id,
                payload,
            } => match payload {
                StreamEvent::Done {
                    run_id,
                    input_message_id,
                    payload,
                    encrypted_payload,
                    ..
                } => {
                    assert_eq!(session_id, Some(Uuid::nil()));
                    assert_eq!(run_id.as_deref(), Some("run-1"));
                    assert_eq!(input_message_id, Some(Uuid::nil()));
                    assert_eq!(
                        payload.as_ref().and_then(|value| value.as_str()),
                        Some("result")
                    );
                    assert!(encrypted_payload.is_none());
                }
                _ => panic!("wrong stream event"),
            },
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn response_agent_response_with_encrypted_done_roundtrip() {
        let resp = Response::AgentResponse {
            session_id: Some(Uuid::nil()),
            payload: StreamEvent::Done {
                run_id: Some("run-1".into()),
                input_message_id: Some(Uuid::nil()),
                payload: Some(serde_json::Value::String("compat".into())),
                encrypted_payload: Some(EncryptedPayload {
                    account_id: Uuid::nil(),
                    encryption_scope: None,
                    object_id: Uuid::new_v4(),
                    object_type: "agent_response".into(),
                    algorithm: "aes-256-gcm".into(),
                    key_version: 1,
                    nonce: "bm9uY2U=".into(),
                    ciphertext: "Y2lwaGVydGV4dA==".into(),
                }),
                total_input_tokens: 0,
                total_output_tokens: 0,
                project: None,
                agent: None,
                session_id: None,
            },
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains(r#""encrypted_payload""#));

        let parsed: Response = serde_json::from_str(&json).unwrap();
        match parsed {
            Response::AgentResponse { payload, .. } => match payload {
                StreamEvent::Done {
                    encrypted_payload, ..
                } => {
                    assert!(encrypted_payload.is_some());
                }
                _ => panic!("wrong stream event"),
            },
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn response_push_notification_roundtrip() {
        let payload = EncryptedPayload {
            account_id: Uuid::new_v4(),
            encryption_scope: Some("org".into()),
            object_id: Uuid::new_v4(),
            object_type: "push.notification".into(),
            algorithm: "aes-256-gcm".into(),
            key_version: 1,
            nonce: "bm9uY2U=".into(),
            ciphertext: "Y2lwaGVydGV4dA==".into(),
        };
        let session_id = Uuid::new_v4();
        let recipient_user_id = Uuid::new_v4();
        let resp = Response::PushNotification {
            agent: "reviewer".into(),
            session_id,
            recipient_user_id: Some(recipient_user_id),
            recipient_handle: Some("@casey".into()),
            encrypted_payload: payload.clone(),
        };

        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains(r#""type":"push.notification""#));
        assert!(json.contains(r#""session_id""#));
        assert!(json.contains(r#""recipient_handle":"@casey""#));
        assert!(json.contains(r#""encrypted_payload""#));

        let parsed: Response = serde_json::from_str(&json).unwrap();
        match parsed {
            Response::PushNotification {
                agent,
                session_id: parsed_session_id,
                recipient_user_id: parsed_recipient_user_id,
                recipient_handle,
                encrypted_payload,
            } => {
                assert_eq!(agent, "reviewer");
                assert_eq!(parsed_session_id, session_id);
                assert_eq!(parsed_recipient_user_id, Some(recipient_user_id));
                assert_eq!(recipient_handle.as_deref(), Some("@casey"));
                assert_eq!(encrypted_payload.account_id, payload.account_id);
                assert_eq!(encrypted_payload.encryption_scope.as_deref(), Some("org"));
                assert_eq!(encrypted_payload.object_type, "push.notification");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn response_worker_heartbeat_roundtrip() {
        let resp = Response::WorkerHeartbeat {
            worker_id: Uuid::nil(),
            capabilities: vec![crate::Capability::Chat, crate::Capability::Task],
            version: Some("0.1.0".into()),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains(r#""type":"worker.heartbeat""#));
        assert!(json.contains(r#""worker_id""#));
        assert!(json.contains(r#""capabilities""#));
        assert!(json.contains(r#""version":"0.1.0""#));
        let parsed: Response = serde_json::from_str(&json).unwrap();
        match parsed {
            Response::WorkerHeartbeat {
                worker_id,
                capabilities,
                version,
            } => {
                assert_eq!(worker_id, Uuid::nil());
                assert_eq!(capabilities.len(), 2);
                assert_eq!(version.as_deref(), Some("0.1.0"));
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn response_worker_heartbeat_without_version() {
        // Backward compat: old workers don't send version.
        let json = r#"{"type":"worker.heartbeat","worker_id":"00000000-0000-0000-0000-000000000000","capabilities":["chat"]}"#;
        let parsed: Response = serde_json::from_str(json).unwrap();
        match parsed {
            Response::WorkerHeartbeat { version, .. } => {
                assert!(version.is_none());
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn response_worker_registered_roundtrip() {
        let resp = Response::WorkerRegistered {
            worker_id: Uuid::nil(),
            capabilities: vec![crate::Capability::Manifest],
            version: Some("0.2.0".into()),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains(r#""type":"worker.registered""#));
        let parsed: Response = serde_json::from_str(&json).unwrap();
        match parsed {
            Response::WorkerRegistered {
                capabilities,
                version,
                ..
            } => {
                assert_eq!(capabilities, vec![crate::Capability::Manifest]);
                assert_eq!(version.as_deref(), Some("0.2.0"));
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn task_execution_state_roundtrip_encodes_typed_failure_and_origin() {
        let response = Response::TaskExecutionState {
            execution_run_id: Uuid::nil(),
            task_id: Uuid::nil(),
            state: TaskExecutionState::Failed {
                error: "provider unavailable".to_string(),
            },
            origin: TaskExecutionOrigin::Schedule {
                schedule_id: Uuid::nil(),
                scheduled_for: "2026-07-16T12:00:00Z".to_string(),
                next_run_at: None,
                assignment_revision: "2026-07-16T11:00:00Z".to_string(),
                project: Some("demo".to_string()),
                target: crate::TaskExecutionTarget::Routine("daily-review".to_string()),
            },
            revision: 2,
            recovered: false,
        };

        let json = serde_json::to_value(&response).unwrap();
        assert_eq!(json["state"]["status"], "failed");
        assert_eq!(json["state"]["error"], "provider unavailable");
        assert_eq!(json["origin"]["kind"], "schedule");
        assert_eq!(json["origin"]["target"]["kind"], "routine");
        assert!(serde_json::from_value::<Response>(json).is_ok());
    }

    #[test]
    fn response_delivery_receipt_roundtrip() {
        let resp = Response::DeliveryReceipt {
            message_id: "msg-42".into(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains(r#""type":"delivery_receipt""#));
        let parsed: Response = serde_json::from_str(&json).unwrap();
        match parsed {
            Response::DeliveryReceipt { message_id } => assert_eq!(message_id, "msg-42"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn response_repo_sync_complete_roundtrip() {
        let resp = Response::RepoSyncComplete {
            project: "demo_project".into(),
            success: false,
            error: Some("clone failed".into()),
        };
        let json = serde_json::to_string(&resp).unwrap();
        let parsed: Response = serde_json::from_str(&json).unwrap();
        match parsed {
            Response::RepoSyncComplete { success, error, .. } => {
                assert!(!success);
                assert_eq!(error.unwrap(), "clone failed");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn command_task_execute_roundtrip() {
        let cmd = Command::TaskExecute {
            task_id: Uuid::nil(),
            project: Some("demo_project".into()),
            execution_run_id: Uuid::nil(),
            trigger: TaskExecutionTrigger::Manual,
            target: crate::TaskExecutionTarget::Agent("coder".into()),
            payload: Some(crate::TaskExecuteContent {
                title: "Fix bug".into(),
                instructions: Some("In auth module".into()),
                slug: None,
                labels: vec!["urgent".into()],
                status: None,
                priority: None,
            }),
            encrypted_payload: None,
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains(r#""type":"task.execute""#));
        let parsed: Command = serde_json::from_str(&json).unwrap();
        match parsed {
            Command::TaskExecute {
                payload: Some(payload),
                ..
            } => {
                assert_eq!(payload.title, "Fix bug");
                assert_eq!(payload.labels, vec!["urgent"]);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn command_manifest_changed_roundtrip() {
        let cmd = Command::ManifestChanged {
            schema: "manifest.changed.v1".into(),
            resource_id: uuid::Uuid::nil(),
            resource_type: crate::ResourceType::Agent,
            resource: "demo_agent".into(),
            action: crate::ResourceAction::Updated,
            project: None,
            payload: None,
            encrypted_payload: None,
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains(r#""type":"manifest.changed""#));
        let parsed: Command = serde_json::from_str(&json).unwrap();
        match parsed {
            Command::ManifestChanged {
                resource_type,
                action,
                ..
            } => {
                assert_eq!(resource_type, crate::ResourceType::Agent);
                assert_eq!(action, crate::ResourceAction::Updated);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn envelope_roundtrip() {
        let cmd = Command::ExecutionCancel {
            execution_run_id: Uuid::nil(),
        };
        let payload = serde_json::to_value(&cmd).unwrap();
        let env = Envelope::new(Uuid::nil(), payload);
        let json = serde_json::to_string(&env).unwrap();
        let parsed: Envelope = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.message_id, env.message_id);
        assert_eq!(parsed.attempt, 1);
    }
}
