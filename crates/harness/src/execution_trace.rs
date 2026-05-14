//! Storage-neutral execution trace runtime hooks.
//!
//! The harness owns when trace events are emitted and the trace data model. Host
//! crates own where those traces are stored.

use async_trait::async_trait;
use nenjo::TurnEvent;
use uuid::Uuid;

pub use crate::trace::{
    AbilityExecutionTrace, AbilityInvocationTrace, AgentExecutionTrace, DelegationExecutionTrace,
    DelegationInvocationTrace, TaskTraceLocation, TraceEvent, TraceFlushTarget, TraceMode,
    TraceRecordUpdate, TraceRecorderCore,
};

#[derive(Debug, Clone)]
/// Agent identity attached to an execution trace.
pub struct TraceAgent {
    pub id: Uuid,
    pub name: String,
}

#[derive(Debug, Clone)]
/// Logical destination for a trace writer.
pub enum ExecutionTraceTarget {
    Chat {
        session_id: Uuid,
        project_slug: String,
    },
    Task {
        project_slug: String,
        task_slug: String,
        step_name: Option<String>,
        step_id: Option<Uuid>,
    },
}

#[async_trait]
/// Records turn events for a single trace target.
pub trait ExecutionTraceWriter: Send + Sync {
    /// Record one agent turn event.
    fn record(&self, event: &TurnEvent);

    /// Mark the trace as failed before it is flushed.
    fn finalize_with_error(&self, error: &str);

    /// Flush and finish the writer.
    async fn finish(self);
}

/// Host-provided trace runtime.
///
/// The associated writer keeps trace recording statically dispatched for each
/// concrete runtime.
pub trait ExecutionTraceRuntime: Send + Sync {
    type Writer: ExecutionTraceWriter;

    /// Return the stable platform trace reference for a target, if one exists.
    fn trace_ref(&self, target: &ExecutionTraceTarget, agent: &TraceAgent) -> Option<String>;

    /// Create a writer for a trace target.
    fn writer(&self, target: ExecutionTraceTarget, agent: TraceAgent) -> Self::Writer;
}

/// No-op trace runtime used when a host does not configure trace persistence.
pub struct NoopExecutionTraceRuntime;

impl ExecutionTraceRuntime for NoopExecutionTraceRuntime {
    type Writer = NoopExecutionTraceWriter;

    fn trace_ref(&self, _target: &ExecutionTraceTarget, _agent: &TraceAgent) -> Option<String> {
        None
    }

    fn writer(&self, _target: ExecutionTraceTarget, _agent: TraceAgent) -> Self::Writer {
        NoopExecutionTraceWriter
    }
}

impl<T> ExecutionTraceRuntime for std::sync::Arc<T>
where
    T: ExecutionTraceRuntime + ?Sized,
{
    type Writer = T::Writer;

    fn trace_ref(&self, target: &ExecutionTraceTarget, agent: &TraceAgent) -> Option<String> {
        (**self).trace_ref(target, agent)
    }

    fn writer(&self, target: ExecutionTraceTarget, agent: TraceAgent) -> Self::Writer {
        (**self).writer(target, agent)
    }
}

/// No-op trace writer used by [`NoopExecutionTraceRuntime`].
pub struct NoopExecutionTraceWriter;

#[async_trait]
impl ExecutionTraceWriter for NoopExecutionTraceWriter {
    fn record(&self, _event: &TurnEvent) {}

    fn finalize_with_error(&self, _error: &str) {}

    async fn finish(self) {}
}
