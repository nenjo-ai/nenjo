//! Cron execution wrapper — runs one scheduled routine firing.

use std::time::Duration;

use anyhow::Result;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info};

use crate::manifest::RoutineManifest;
use crate::provider::ProviderRuntime;
use crate::routines::RoutineEvent;
use crate::routines::types::{CronSchedule, RoutineState, StepResult};

pub(crate) struct CronExecutionConfig<'a> {
    pub events_tx: &'a mpsc::UnboundedSender<RoutineEvent>,
    pub cancel: &'a CancellationToken,
    pub schedule: &'a CronSchedule,
    pub start_at: Option<chrono::DateTime<chrono::Utc>>,
    pub timeout: Duration,
}

/// Execute one cron routine firing. External schedulers are responsible for
/// starting subsequent firings.
pub(crate) async fn execute_routine_cron<P>(
    provider: &P,
    routine: &RoutineManifest,
    state: &mut RoutineState,
    config: CronExecutionConfig<'_>,
) -> Result<StepResult>
where
    P: ProviderRuntime,
{
    let CronExecutionConfig {
        events_tx,
        cancel,
        schedule: _schedule,
        start_at,
        timeout,
    } = config;

    info!(
        routine = %routine.name,
        timeout_secs = timeout.as_secs(),
        "Starting cron routine execution"
    );

    if let Some(start_at) = start_at {
        let delay = (start_at - chrono::Utc::now())
            .to_std()
            .unwrap_or(Duration::ZERO);
        if !delay.is_zero() {
            debug!(routine = %routine.name, delay_secs = delay.as_secs(), "Waiting for restored cron schedule");
            tokio::select! {
                _ = tokio::time::sleep(delay) => {}
                _ = cancel.cancelled() => {
                    info!(routine = %routine.name, "Cron routine cancelled before first restored run");
                    return Ok(StepResult {
                        passed: false,
                        output: format!("Cron routine '{}' cancelled before first cycle", routine.name),
                        ..Default::default()
                    });
                }
            }
        }
    }

    let cycle = 1;
    let _ = events_tx.send(RoutineEvent::CronCycleStarted { cycle });

    let result = crate::routines::executor::execute_routine_once(
        provider, routine, state, events_tx, cancel,
    )
    .await?;

    let _ = events_tx.send(RoutineEvent::CronCycleCompleted {
        cycle,
        result: result.clone(),
        total_input_tokens: state.metrics.total_input_tokens(),
        total_output_tokens: state.metrics.total_output_tokens(),
    });

    info!(
        cycle,
        routine = %routine.name,
        passed = result.passed,
        "Cron routine firing completed"
    );

    Ok(result)
}
