//! Cron poll loop — wraps a routine in a repeating execution cycle.

use std::time::Duration;

use anyhow::Result;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info};

use crate::manifest::RoutineManifest;
use crate::provider::Provider;
use crate::routines::RoutineEvent;
use crate::routines::types::{CronSchedule, RoutineState, StepResult};

pub(crate) struct CronExecutionConfig<'a> {
    pub events_tx: &'a mpsc::UnboundedSender<RoutineEvent>,
    pub cancel: &'a CancellationToken,
    pub schedule: &'a CronSchedule,
    pub start_at: Option<chrono::DateTime<chrono::Utc>>,
    pub timeout: Duration,
}

/// Execute a routine on a repeating schedule until a completion signal
/// is received, the timeout expires, or the execution is cancelled.
pub(crate) async fn execute_routine_cron(
    provider: &Provider,
    routine: &RoutineManifest,
    state: &mut RoutineState,
    config: CronExecutionConfig<'_>,
) -> Result<StepResult> {
    let CronExecutionConfig {
        events_tx,
        cancel,
        schedule,
        start_at,
        timeout,
    } = config;
    let deadline = tokio::time::Instant::now() + timeout;
    let mut cycle = 0u32;
    let mut last_result;

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

    loop {
        cycle += 1;

        let _ = events_tx.send(RoutineEvent::CronCycleStarted { cycle });

        // Run the full routine once
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

        last_result = result;

        // Check for a structured verdict in the final step result.
        // A gate_verdict tool call in any step produces {"verdict": "pass"|"fail"}
        // in the step's data, which propagates to the routine result.
        if let Some(verdict) = last_result.data.get("verdict").and_then(|v| v.as_str()) {
            match verdict {
                "pass" => {
                    info!(cycle, routine = %routine.name, "Cron routine step completed (pass)");
                    return Ok(last_result);
                }
                "fail" => {
                    info!(cycle, routine = %routine.name, "Cron routine step completed (fail)");
                    last_result.passed = false;
                    return Ok(last_result);
                }
                _ => {}
            }
        } else {
            debug!(cycle, routine = %routine.name, "No verdict, will retry");
        }

        // Check timeout
        if tokio::time::Instant::now() >= deadline {
            info!(cycle, routine = %routine.name, "Cron routine timed out");
            return Ok(StepResult {
                passed: false,
                output: format!(
                    "Cron routine '{}' timed out after {} cycles ({}s)",
                    routine.name,
                    cycle,
                    timeout.as_secs()
                ),
                ..Default::default()
            });
        }

        // Reset step results for next cycle
        state.step_results.clear();

        // Cancellable sleep between cycles — computed dynamically for cron
        // expressions so the next fire aligns with the wall-clock schedule.
        let delay = schedule.next_delay();
        debug!(
            cycle,
            delay_secs = delay.as_secs(),
            "Sleeping until next cron cycle"
        );
        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            _ = cancel.cancelled() => {
                info!(cycle, routine = %routine.name, "Cron routine cancelled");
                return Ok(StepResult {
                    passed: false,
                    output: format!("Cron routine '{}' cancelled at cycle {}", routine.name, cycle),
                    ..Default::default()
                });
            }
        }
    }
}
