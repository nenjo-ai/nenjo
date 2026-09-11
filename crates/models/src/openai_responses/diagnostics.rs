//! Content-free timing and provider identity for one Responses HTTP attempt.

use std::time::{Duration, Instant};

use serde_json::Value;
use tracing::debug;

use crate::{ChatResponse, ResponseTerminationError};

pub(crate) struct ResponseDiagnostics {
    started: Instant,
    first_event: Option<Duration>,
    first_delta: Option<Duration>,
    response_id: Option<String>,
}

impl Default for ResponseDiagnostics {
    fn default() -> Self {
        Self {
            started: Instant::now(),
            first_event: None,
            first_delta: None,
            response_id: None,
        }
    }
}

impl ResponseDiagnostics {
    /// Mark the first text or reasoning delta separately from lifecycle events.
    pub(crate) fn observe_delta(&mut self) {
        self.first_delta
            .get_or_insert_with(|| self.started.elapsed());
    }

    /// Observe parsed events, excluding transport heartbeats from first-event timing.
    pub(crate) fn observe(&mut self, value: &Value) {
        self.first_event
            .get_or_insert_with(|| self.started.elapsed());
        if let Some(id) = value
            .pointer("/response/id")
            .or_else(|| value.get("id"))
            .and_then(Value::as_str)
            && self.response_id.as_deref() != Some(id)
        {
            self.response_id = Some(id.to_owned());
            debug!(
                target: "nenjo_models::responses",
                provider_response_id = id,
                "Responses provider identity received"
            );
        }
    }

    /// Log totals on both success and failure without logging generated content.
    pub(crate) fn finish(&self, result: &anyhow::Result<ChatResponse>) {
        let termination = result
            .as_ref()
            .err()
            .and_then(|error| error.downcast_ref::<ResponseTerminationError>());
        let usage = result
            .as_ref()
            .ok()
            .map(|response| response.usage)
            .or_else(|| termination.map(|error| error.usage));
        debug!(
            target: "nenjo_models::responses",
            provider_response_id = self.response_id.as_deref(),
            first_event_ms = self.first_event.map(|elapsed| elapsed.as_millis() as u64),
            first_delta_ms = self.first_delta.map(|elapsed| elapsed.as_millis() as u64),
            total_duration_ms = self.started.elapsed().as_millis() as u64,
            success = result.is_ok(),
            termination = ?termination.map(|error| error.reason),
            input_tokens = usage.map(|usage| usage.input_tokens),
            output_tokens = usage.map(|usage| usage.output_tokens),
            cached_input_tokens = usage.and_then(|usage| usage.cached_input_tokens),
            reasoning_tokens = usage.and_then(|usage| usage.reasoning_tokens),
            "Responses request finished"
        );
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn identity_survives_delta_events_and_first_event_time_is_stable() {
        let mut diagnostics = ResponseDiagnostics::default();
        diagnostics.observe(&json!({"response":{"id":"resp-1"}}));
        let first_event = diagnostics.first_event;
        diagnostics.observe(&json!({"type":"response.output_text.delta","delta":"hello"}));
        diagnostics.observe_delta();
        let first_delta = diagnostics.first_delta;
        diagnostics.observe_delta();
        assert_eq!(diagnostics.response_id.as_deref(), Some("resp-1"));
        assert_eq!(diagnostics.first_event, first_event);
        assert_eq!(diagnostics.first_delta, first_delta);
        assert!(first_delta >= first_event);
    }
}
