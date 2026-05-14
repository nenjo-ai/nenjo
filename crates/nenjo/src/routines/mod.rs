//! Routine execution — DAG-based execution pipelines.
//!
//! Routines are directed acyclic graphs of steps connected by conditional edges.
//! Each step can be an agent task, gate evaluation, council delegation, or
//! terminal node.
//!
//! ```ignore
//! // One-shot task execution
//! let task = nenjo::TaskInput::new(project_id, task_id, "Fix auth", "Repair the login flow");
//! let result = provider.routine_by_id(routine_id)?.run(task).await?;
//!
//! // Streaming execution with events
//! let mut handle = provider.routine_by_id(routine_id)?
//!     .run_stream(task)
//!     .await?;
//!
//! // Cancel a running cron
//! handle.cancel();
//! ```

pub mod council;
pub mod cron;
pub mod event;
pub mod executor;
pub mod gate;
pub mod runner;
pub mod types;

use crate::AgentBuilder;
use crate::manifest::RoutineStepManifest;
use crate::memory::MemoryScope;
use crate::provider::ProviderRuntime;

pub use event::RoutineEvent;
pub use runner::{RoutineExecutionHandle, RoutineRunner};
pub use types::{
    CronMode, CronStepConfig, EdgeCondition, LambdaStepConfig, RoutineInput, RoutineMetrics,
    SessionBinding, StepMetrics, StepResult, StepType,
};

pub(crate) fn with_agent_step_tools<P>(builder: AgentBuilder<P>) -> AgentBuilder<P>
where
    P: ProviderRuntime,
{
    builder.with_tool(gate::PassVerdictTool::new())
}

const DEFAULT_ROUTINE_STEP_MAX_TURNS: usize = 50;

pub(crate) fn with_routine_step_max_turns<P>(
    builder: AgentBuilder<P>,
    step: &RoutineStepManifest,
) -> AgentBuilder<P>
where
    P: ProviderRuntime,
{
    let configured = step
        .config
        .get("max_turns")
        .and_then(|value| {
            value
                .as_u64()
                .or_else(|| value.as_str().and_then(|raw| raw.parse::<u64>().ok()))
        })
        .and_then(|value| usize::try_from(value).ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_ROUTINE_STEP_MAX_TURNS);

    builder.with_max_turns(configured)
}

pub(crate) fn apply_session_binding_memory_scope<P>(
    builder: AgentBuilder<P>,
    binding: Option<&SessionBinding>,
) -> AgentBuilder<P>
where
    P: ProviderRuntime,
{
    let Some(binding) = binding else {
        return builder;
    };
    let Some(namespace) = binding.memory_namespace.as_deref() else {
        return builder;
    };
    let Some(scope) = MemoryScope::from_namespace(namespace) else {
        return builder;
    };
    builder.with_memory_scope(scope)
}
