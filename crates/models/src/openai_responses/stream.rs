//! Responses SSE lifecycle handling. Only a completed response can yield tool calls.

use std::collections::HashSet;

use anyhow::Context;
use futures_util::StreamExt;
use serde_json::Value;
use tokio::sync::mpsc::Sender;

use super::diagnostics::ResponseDiagnostics;
use super::{
    ResponsesResponse, ResponsesUsage, response_text, response_tool_calls, response_usage,
};
use crate::sse::{decode_event, take_frame};
use crate::{
    ChatResponse, FinishReason, ProviderStreamEvent, ResponseTermination, ResponseTerminationError,
};

/// Decode the terminal response without accepting partial or failed generations.
pub(crate) fn completed_response(value: Value) -> anyhow::Result<ChatResponse> {
    let status = value.get("status").and_then(Value::as_str);
    if status != Some("completed") {
        return Err(termination_error(&value, status.unwrap_or("missing")).into());
    }
    let response: ResponsesResponse =
        serde_json::from_value(value).context("invalid completed Responses payload")?;
    // A malformed final function item must fail closed, never invent an ID or
    // execute partial arguments. Deltas and item.done are intentionally not executable.
    let mut call_ids = HashSet::new();
    for item in &response.output {
        if item.kind.as_deref() == Some("function_call") {
            anyhow::ensure!(
                item.name.as_deref().is_some_and(|value| !value.is_empty()),
                "Responses function call omitted its name"
            );
            anyhow::ensure!(
                item.call_id
                    .as_deref()
                    .is_some_and(|value| !value.is_empty()),
                "Responses function call omitted its call_id"
            );
            anyhow::ensure!(
                call_ids.insert(item.call_id.as_deref()),
                "Responses output contains duplicate function call IDs"
            );
            anyhow::ensure!(
                item.status
                    .as_deref()
                    .is_none_or(|status| status == "completed"),
                "Responses function call did not complete"
            );
            let arguments = item
                .arguments
                .as_ref()
                .context("Responses function call omitted its arguments")?;
            let decoded = match arguments {
                Value::String(text) => serde_json::from_str::<Value>(text)
                    .context("Responses function call arguments are invalid JSON")?,
                value => value.clone(),
            };
            anyhow::ensure!(
                decoded.is_object(),
                "Responses function call arguments must be a JSON object"
            );
        }
    }
    let tool_calls = response_tool_calls(&response);
    let text = response_text(&response);
    anyhow::ensure!(
        text.is_some() || !tool_calls.is_empty(),
        "Completed Responses payload contains no assistant text or function calls"
    );
    Ok(ChatResponse {
        text,
        finish_reason: if tool_calls.is_empty() {
            FinishReason::Stop
        } else {
            FinishReason::ToolCalls
        },
        tool_calls,
        provider_tool_calls: Vec::new(),
        usage: response_usage(&response),
    })
}

fn error_message(value: &Value) -> &str {
    value
        .pointer("/error/message")
        .or_else(|| value.pointer("/incomplete_details/reason"))
        .or_else(|| value.get("message"))
        .and_then(Value::as_str)
        .unwrap_or("generation did not complete")
}

/// Classify terminal failures before reading output, so partial calls cannot escape.
fn termination_error(value: &Value, status: &str) -> ResponseTerminationError {
    let reason = match status {
        "cancelled" | "canceled" => ResponseTermination::Cancelled,
        "failed" | "error" => ResponseTermination::Failed,
        _ => match value
            .pointer("/incomplete_details/reason")
            .and_then(Value::as_str)
        {
            Some("max_output_tokens") => ResponseTermination::OutputLimit,
            Some("content_filter") => ResponseTermination::ContentFilter,
            _ => ResponseTermination::Incomplete,
        },
    };
    ResponseTerminationError {
        reason,
        response_id: value.get("id").and_then(Value::as_str).map(str::to_owned),
        usage: value
            .get("usage")
            .cloned()
            .and_then(|usage| serde_json::from_value::<ResponsesUsage>(usage).ok())
            .map(|usage| usage.token_usage())
            .unwrap_or_default(),
        message: error_message(value).to_owned(),
    }
}

/// Read complete SSE frames as bytes, honoring cancellation and channel backpressure.
/// `response.completed` finishes immediately even if the server leaves HTTP open.
pub(crate) async fn read_stream(
    response: reqwest::Response,
    events: Option<&Sender<ProviderStreamEvent>>,
    diagnostics: &mut ResponseDiagnostics,
) -> anyhow::Result<ChatResponse> {
    let mut body = response.bytes_stream();
    let mut buffer = Vec::new();
    loop {
        let chunk = if let Some(events) = events {
            tokio::select! {
                biased;
                () = events.closed() => anyhow::bail!("provider stream consumer closed"),
                chunk = body.next() => chunk,
            }
        } else {
            body.next().await
        };
        let Some(chunk) = chunk else {
            anyhow::bail!("Responses stream ended before response.completed");
        };
        buffer.extend_from_slice(&chunk.context("Responses stream read error")?);
        while let Some(frame) = take_frame(&mut buffer) {
            if let Some(response) = absorb_frame(&frame, events, diagnostics).await? {
                return Ok(response);
            }
        }
    }
}

async fn absorb_frame(
    frame: &[u8],
    events: Option<&Sender<ProviderStreamEvent>>,
    diagnostics: &mut ResponseDiagnostics,
) -> anyhow::Result<Option<ChatResponse>> {
    let Some(event) = decode_event(frame)? else {
        return Ok(None);
    };
    anyhow::ensure!(
        event.data.trim() != "[DONE]",
        "Responses stream received [DONE] before response.completed"
    );
    let value: Value =
        serde_json::from_str(&event.data).context("invalid Responses stream event JSON")?;
    diagnostics.observe(&value);
    let kind = value
        .get("type")
        .and_then(Value::as_str)
        .or(event.name.as_deref())
        .context("Responses stream event omitted its type")?;
    let delta = match kind {
        "response.completed" => {
            return completed_response(
                value
                    .get("response")
                    .cloned()
                    .context("response.completed omitted its response")?,
            )
            .map(Some);
        }
        "response.failed" | "response.incomplete" | "response.cancelled" | "error" => {
            return Err(termination_error(
                value.get("response").unwrap_or(&value),
                kind.strip_prefix("response.").unwrap_or(kind),
            )
            .into());
        }
        "response.output_text.delta" => Some(false),
        "response.reasoning_text.delta" | "response.reasoning_summary_text.delta" => Some(true),
        // Progress and function argument deltas are informational. The terminal
        // response contains authoritative output, including call IDs and usage.
        _ => None,
    };
    if let Some(reasoning) = delta {
        let text = value
            .get("delta")
            .and_then(Value::as_str)
            .context("Responses delta omitted its text")?;
        if !text.is_empty() {
            diagnostics.observe_delta();
            if let Some(events) = events {
                let event = if reasoning {
                    ProviderStreamEvent::ReasoningDelta(text.to_owned())
                } else {
                    ProviderStreamEvent::TextDelta(text.to_owned())
                };
                events
                    .send(event)
                    .await
                    .context("provider stream consumer closed")?;
            }
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests;
