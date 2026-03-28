//! Cron poll loop — wraps a routine in a repeating execution cycle.

use std::time::Duration;

use anyhow::Result;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info};

use crate::manifest::RoutineManifest;
use crate::provider::Provider;
use crate::routines::RoutineEvent;
use crate::routines::types::{RoutineState, StepResult};

/// Execute a routine on a repeating interval until a completion signal
/// is received, the timeout expires, or the execution is cancelled.
pub(crate) async fn execute_routine_cron(
    provider: &Provider,
    routine: &RoutineManifest,
    state: &mut RoutineState,
    events_tx: &mpsc::UnboundedSender<RoutineEvent>,
    cancel: &CancellationToken,
    interval: Duration,
    timeout: Duration,
) -> Result<StepResult> {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut cycle = 0u32;
    let mut last_result;

    info!(
        routine = %routine.name,
        interval_secs = interval.as_secs(),
        timeout_secs = timeout.as_secs(),
        "Starting cron routine execution"
    );

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
                    info!(cycle, routine = %routine.name, "Cron routine completed (pass)");
                    return Ok(last_result);
                }
                "fail" => {
                    info!(cycle, routine = %routine.name, "Cron routine completed (fail)");
                    last_result.passed = false;
                    return Ok(last_result);
                }
                _ => {
                    debug!(cycle, routine = %routine.name, verdict, "Unknown verdict, will retry");
                }
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

        // Cancellable sleep between cycles
        tokio::select! {
            _ = tokio::time::sleep(interval) => {}
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
