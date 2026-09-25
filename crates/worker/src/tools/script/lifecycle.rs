//! Async-operation lifecycle for promoted scripts.
//!
//! A script that outlives its synchronous phase is promoted to an
//! `AsyncOperationKind::Script` operation with inspect/stop/wait controls,
//! mirroring the shell tool. This module owns the promotion receipt, the
//! log-streaming finisher, and the stop→cancellation bridge.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use nenjo::tools::{AsyncControl, AsyncControls, AsyncOperationKind, AsyncOperationStartReceipt};
use nenjo::{
    AsyncOperationHandle, AsyncOperationRuntime, AsyncOperationTranscriptEvent, StartAsyncOperation,
};
use nenjo_tool_api::ToolResult;
use serde::Serialize;
use tokio_util::sync::CancellationToken;

use super::SCRIPT_TOOL_NAME;
use super::engine::{LogBuffer, ScriptOutcome};

/// How long a script runs synchronously before promotion to an async operation.
pub(crate) const INITIAL_WAIT: Duration = Duration::from_secs(2);
/// Poll interval for streaming `ctx.log` lines into the operation transcript.
const LOG_STREAM_INTERVAL: Duration = Duration::from_millis(300);

static SCRIPT_OPERATION_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Promote a still-running script to an async operation and return a receipt.
pub(crate) async fn promote_to_operation(
    runtime: AsyncOperationRuntime,
    join: tokio::task::JoinHandle<anyhow::Result<ScriptOutcome>>,
    stop: CancellationToken,
    logs: Arc<Mutex<LogBuffer>>,
    script: &str,
    timeout: Duration,
) -> anyhow::Result<ToolResult> {
    let operation_id = format!(
        "script_{}",
        SCRIPT_OPERATION_COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    let controls = AsyncControls::new(AsyncControl::Inspect)
        .with(AsyncControl::Stop)
        .with(AsyncControl::Wait);
    let handle = runtime
        .start(StartAsyncOperation {
            id: operation_id.clone(),
            kind: AsyncOperationKind::Script,
            label: script_operation_label(script),
            parent_operation_id: None,
            parent_tool_name: Some(SCRIPT_TOOL_NAME.into()),
            started_summary: "Script is still running".into(),
            model_visible: true,
            controls,
        })
        .await;

    // Route async-operation stop (model-facing stop tool or runtime
    // cancellation) into the engine's cancellation token.
    let bridge_stop = stop.clone();
    let operation_stop = handle.cancel_token();
    tokio::spawn(async move {
        operation_stop.cancelled().await;
        bridge_stop.cancel();
    });

    stream_new_logs(&handle, &logs).await;
    let finisher_handle = handle.clone();
    let finisher = tokio::spawn(async move {
        finish_script_operation(finisher_handle, join, logs).await;
    });
    handle.attach_join(finisher).await;

    Ok(ToolResult::success(serde_json::to_string(
        &ScriptOperationStarted {
            result_type: "operation_started",
            async_operation: AsyncOperationStartReceipt::new(
                operation_id,
                AsyncOperationKind::Script,
                controls,
            ),
            timeout_ms: timeout.as_millis() as u64,
        },
    )?))
}

/// Await the engine, stream log lines into the transcript, and settle the
/// async operation with the final output.
async fn finish_script_operation(
    handle: AsyncOperationHandle,
    mut join: tokio::task::JoinHandle<anyhow::Result<ScriptOutcome>>,
    logs: Arc<Mutex<LogBuffer>>,
) {
    let mut interval = tokio::time::interval(LOG_STREAM_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let result = loop {
        tokio::select! {
            res = &mut join => break res,
            _ = interval.tick() => stream_new_logs(&handle, &logs).await,
        }
    };
    stream_new_logs(&handle, &logs).await;
    let drained = logs.lock().expect("log mutex poisoned").drain_all();
    match result {
        // The engine already sets `outcome.error` (including the timeout
        // message) whenever a run fails, times out, or is stopped.
        Ok(Ok(outcome)) if outcome.error.is_none() => {
            handle
                .complete(
                    "Script completed",
                    Some(super::envelope(&outcome, &drained)),
                )
                .await;
        }
        Ok(Ok(outcome)) => {
            handle
                .fail_with_output(
                    outcome
                        .error
                        .clone()
                        .unwrap_or_else(|| "script failed".into()),
                    Some(super::envelope(&outcome, &drained)),
                )
                .await;
        }
        Ok(Err(error)) => handle.fail(error.to_string()).await,
        Err(error) => handle.fail(format!("script task failed: {error}")).await,
    }
}

async fn stream_new_logs(handle: &AsyncOperationHandle, logs: &Arc<Mutex<LogBuffer>>) {
    let lines = logs.lock().expect("log mutex poisoned").take_new();
    for line in lines {
        handle
            .transcript(AsyncOperationTranscriptEvent::OutputChunk {
                summary: format!("[log] {line}"),
            })
            .await;
    }
}

fn script_operation_label(script: &str) -> String {
    let first_line = script.lines().find(|line| !line.trim().is_empty());
    match first_line {
        Some(line) if line.len() <= 60 => format!("script: {line}"),
        Some(line) => format!("script: {}…", &line[..60]),
        None => "script".to_string(),
    }
}

#[derive(Serialize)]
struct ScriptOperationStarted {
    #[serde(rename = "type")]
    result_type: &'static str,
    #[serde(flatten)]
    async_operation: AsyncOperationStartReceipt,
    timeout_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_uses_first_non_empty_line() {
        assert_eq!(
            script_operation_label("\n  ctx.log();\n"),
            "script:   ctx.log();"
        );
    }

    #[test]
    fn label_truncates_long_lines() {
        let long = "x".repeat(100);
        let label = script_operation_label(&long);
        assert_eq!(label.chars().count(), "script: ".chars().count() + 61); // 60 chars + ellipsis
        assert!(label.ends_with('…'));
    }

    #[test]
    fn label_falls_back_for_blank_scripts() {
        assert_eq!(script_operation_label("  \n "), "script");
    }
}
