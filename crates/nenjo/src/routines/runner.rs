//! Routine runner API.

use anyhow::Result;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::input::{RoutineRun, RoutineRunKind};
use crate::manifest::RoutineManifest;
use crate::provider::{ErasedProvider, ProviderRuntime};
use crate::routines::{self, RoutineEvent, SessionBinding, StepResult};

/// A routine resolved from the manifest, ready to execute.
///
/// Created via [`Provider::routine_by_id`](crate::provider::Provider::routine_by_id).
/// Provides the same simple/streaming split as
/// [`AgentRunner`](crate::AgentRunner).
///
/// ```ignore
/// let task = nenjo::TaskInput::new(project_id, task_id, "Fix auth", "Repair the login flow");
/// let result = provider.routine_by_id(id)?
///     .run(task)
///     .await?;
/// ```
pub struct RoutineRunner<P = ErasedProvider> {
    provider: P,
    routine: RoutineManifest,
    session_binding: Option<SessionBinding>,
}

impl<P> RoutineRunner<P> {
    pub(crate) fn new(provider: P, routine: RoutineManifest) -> Self {
        Self {
            provider,
            routine,
            session_binding: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn provider(&self) -> &P {
        &self.provider
    }

    /// The routine manifest backing this runner.
    pub fn routine(&self) -> &RoutineManifest {
        &self.routine
    }

    /// The routine's name.
    pub fn name(&self) -> &str {
        &self.routine.name
    }

    /// The routine's ID.
    pub fn id(&self) -> Uuid {
        self.routine.id
    }

    /// The session binding applied to runs from this runner, if configured.
    pub fn session_binding(&self) -> Option<&SessionBinding> {
        self.session_binding.as_ref()
    }
}

impl<P> std::fmt::Debug for RoutineRunner<P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RoutineRunner")
            .field("routine_id", &self.routine.id)
            .field("routine_name", &self.routine.name)
            .finish()
    }
}

impl<P> RoutineRunner<P>
where
    P: ProviderRuntime,
{
    /// Apply a session binding to runs started from this runner.
    pub fn with_session_binding(mut self, binding: SessionBinding) -> Self {
        self.session_binding = Some(binding);
        self
    }

    /// Run the routine to completion and return the final result.
    pub async fn run(&self, input: impl Into<RoutineRun>) -> Result<StepResult> {
        self.run_stream(input).await?.output().await
    }

    /// Run the routine with streaming events.
    pub async fn run_stream(&self, input: impl Into<RoutineRun>) -> Result<RoutineExecutionHandle> {
        let mut input = input.into();
        if input.execution.session_binding.is_none() {
            input.execution.session_binding = self.session_binding.clone();
        }
        run_routine_inner(self.provider.clone(), &self.routine, input).await
    }
}

/// Handle to a running routine execution.
///
/// Provides a stream of [`RoutineEvent`]s as the routine progresses, plus
/// access to the final [`StepResult`] when done. Cron routines can be
/// cancelled via [`cancel()`](Self::cancel).
pub struct RoutineExecutionHandle {
    events_rx: mpsc::UnboundedReceiver<RoutineEvent>,
    join: tokio::task::JoinHandle<Result<StepResult>>,
    cancel: CancellationToken,
}

impl RoutineExecutionHandle {
    pub(crate) fn new(
        events_rx: mpsc::UnboundedReceiver<RoutineEvent>,
        join: tokio::task::JoinHandle<Result<StepResult>>,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            events_rx,
            join,
            cancel,
        }
    }

    /// Cancel the running routine. For cron routines, this stops the poll loop
    /// between cycles. For one-shot routines, this stops between DAG steps.
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    /// Receive the next event. Returns `None` when the routine finishes.
    pub async fn recv(&mut self) -> Option<RoutineEvent> {
        self.events_rx.recv().await
    }

    /// Get a mutable reference to the underlying event receiver.
    pub fn events(&mut self) -> &mut mpsc::UnboundedReceiver<RoutineEvent> {
        &mut self.events_rx
    }

    /// Wait for the final result.
    pub async fn output(self) -> Result<StepResult> {
        self.join
            .await
            .map_err(|e| anyhow::anyhow!("routine execution task panicked: {e}"))?
    }
}

async fn run_routine_inner<P>(
    provider: P,
    routine: &RoutineManifest,
    input: RoutineRun,
) -> Result<RoutineExecutionHandle>
where
    P: ProviderRuntime,
{
    let routine = routine.clone();
    let routine_name = routine.name.clone();
    let routine_id = routine.id;
    let cancel = CancellationToken::new();
    let cancel_inner = cancel.clone();

    let cron_schedule = match &input.kind {
        RoutineRunKind::Cron(crate::input::CronInput {
            schedule,
            start_at,
            timeout,
            ..
        }) => Some((schedule.clone(), *start_at, *timeout)),
        RoutineRunKind::Task(_) => None,
    };

    let (events_tx, events_rx) = mpsc::unbounded_channel::<RoutineEvent>();

    let input = routines::types::RoutineInput::from_routine_run(input);
    tracing::debug!(
        is_cron = input.is_cron_trigger,
        "Routine input built from run"
    );

    let join = tokio::spawn(async move {
        let mut state = routines::types::RoutineState::new(routine_id, input);
        state.routine_name = Some(routine_name);

        if let Some((schedule, start_at, timeout)) = cron_schedule {
            routines::cron::executor::execute_routine_cron(
                &provider,
                &routine,
                &mut state,
                routines::cron::executor::CronExecutionConfig {
                    events_tx: &events_tx,
                    cancel: &cancel_inner,
                    schedule: &schedule,
                    start_at,
                    timeout,
                },
            )
            .await
        } else {
            routines::executor::execute_routine(
                &provider,
                &routine,
                &mut state,
                &events_tx,
                &cancel_inner,
            )
            .await
        }
    });

    Ok(RoutineExecutionHandle::new(events_rx, join, cancel))
}
