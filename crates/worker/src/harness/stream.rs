//! Stream event abstractions for the harness.
//!
//! Re-exports `StreamEvent` from the canonical event types and provides a
//! `StreamSender` trait for publishing events to different transports.

use anyhow::Result;
use async_trait::async_trait;

// Re-export the canonical event types.
pub use nenjo_events::Response;
pub use nenjo_events::StreamEvent;

/// Trait for sending stream events to the frontend.
///
/// Implementations forward events over NATS, WebSocket, or discard them (no-op).
#[async_trait]
pub trait StreamSender: Send + Sync {
    /// Send a stream event. Errors are non-fatal and should be logged by the implementation.
    async fn send(&self, event: StreamEvent) -> Result<()>;

    /// Send a raw JSON message (used for step events, completion messages, etc.).
    async fn send_json(&self, json: serde_json::Value) -> Result<()>;
}

/// A no-op sender that discards all events. Used for tests and cron execution.
pub struct NoOpSender;

#[async_trait]
impl StreamSender for NoOpSender {
    async fn send(&self, _event: StreamEvent) -> Result<()> {
        Ok(())
    }

    async fn send_json(&self, _json: serde_json::Value) -> Result<()> {
        Ok(())
    }
}

/// WebSocket-based stream sender for cron execution.
///
/// Wraps a `tokio::sync::broadcast::Sender` for WebSocket delivery.
pub struct WsSender {
    tx: tokio::sync::broadcast::Sender<String>,
}

impl WsSender {
    pub fn new(tx: tokio::sync::broadcast::Sender<String>) -> Self {
        Self { tx }
    }
}

#[async_trait]
impl StreamSender for WsSender {
    async fn send(&self, event: StreamEvent) -> Result<()> {
        if let Ok(json) = serde_json::to_string(&event) {
            let _ = self.tx.send(json);
        }
        Ok(())
    }

    async fn send_json(&self, json: serde_json::Value) -> Result<()> {
        if let Ok(s) = serde_json::to_string(&json) {
            let _ = self.tx.send(s);
        }
        Ok(())
    }
}

/// Legacy event types for backward compatibility.
///
/// TODO: migrate callers to use `nenjo_events::Response` builders directly
pub mod events {
    use serde::Serialize;
    use uuid::Uuid;

    /// Task-related WebSocket message for step/completion events.
    #[derive(Debug, Clone, Serialize)]
    pub struct TaskWsMessage {
        #[serde(rename = "type")]
        pub msg_type: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub execution_run_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub task_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub success: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub error: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub step_name: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub step_type: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub output: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub metadata: Option<serde_json::Value>,
    }

    impl TaskWsMessage {
        /// Create a task.completed message.
        pub fn task_completed(
            execution_run_id: Uuid,
            task_id: Option<Uuid>,
            success: bool,
            error: Option<String>,
            output: Option<String>,
        ) -> Self {
            Self {
                msg_type: "task.completed".to_string(),
                execution_run_id: Some(execution_run_id.to_string()),
                task_id: task_id.map(|id| id.to_string()),
                success: Some(success),
                error,
                step_name: None,
                step_type: None,
                output,
                metadata: None,
            }
        }

        /// Create a step event message (step_started, step_completed, step_failed).
        pub fn step_event(
            execution_run_id: Uuid,
            task_id: Option<Uuid>,
            event_type: &str,
            step_name: &str,
            step_type: &str,
            output: Option<serde_json::Value>,
            metadata: serde_json::Value,
        ) -> Self {
            Self {
                msg_type: format!("task.{event_type}"),
                execution_run_id: Some(execution_run_id.to_string()),
                task_id: task_id.map(|id| id.to_string()),
                success: None,
                error: None,
                step_name: Some(step_name.to_string()),
                step_type: Some(step_type.to_string()),
                output: output.map(|o| o.to_string()),
                metadata: Some(metadata),
            }
        }
    }
}
