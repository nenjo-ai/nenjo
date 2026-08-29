//! Typed delivery modes and handles for interactive chat execution.

use std::fmt::Debug;
use std::marker::PhantomData;

use anyhow::Result;

use super::ExecutionHandle;
use super::types::{PauseToken, TurnEvent, TurnInputSender, TurnOutput};

/// Provider response transport selected for each model request in a turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProviderResponseDelivery {
    Buffered,
    Streaming,
}

mod private {
    pub trait Sealed {
        fn streams_provider_response() -> bool;
    }
}

/// A sealed marker selecting the model transport and public chat event type.
pub trait ChatDelivery: private::Sealed + Copy + Send + Sync + 'static {
    /// Assistant delta payload carried by this delivery mode.
    type Delta: Clone + Debug + Send + Sync + Into<String> + 'static;

    /// Map a provider delta into this delivery mode.
    #[doc(hidden)]
    fn map_delta(delta: String) -> Option<Self::Delta>;
}

/// Buffer every model response and expose no assistant delta payloads.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Buffered;

/// Stream model responses and expose assistant delta payloads as strings.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Streaming;

/// Uninhabited assistant delta type used by [`Buffered`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BufferedDelta {}

impl From<BufferedDelta> for String {
    fn from(delta: BufferedDelta) -> Self {
        match delta {}
    }
}

impl private::Sealed for Buffered {
    fn streams_provider_response() -> bool {
        false
    }
}

impl ChatDelivery for Buffered {
    type Delta = BufferedDelta;

    fn map_delta(_delta: String) -> Option<Self::Delta> {
        None
    }
}

impl private::Sealed for Streaming {
    fn streams_provider_response() -> bool {
        true
    }
}

impl ChatDelivery for Streaming {
    type Delta = String;

    fn map_delta(delta: String) -> Option<Self::Delta> {
        Some(delta)
    }
}

pub(crate) fn provider_response_delivery<D: ChatDelivery>() -> ProviderResponseDelivery {
    match <D as private::Sealed>::streams_provider_response() {
        false => ProviderResponseDelivery::Buffered,
        true => ProviderResponseDelivery::Streaming,
    }
}

/// Typed handle returned by [`AgentRunner::chat`](super::AgentRunner::chat).
///
/// Buffered handles yield `TurnEvent<BufferedDelta>`, whose assistant delta
/// variants are uninhabited. Streaming handles yield ordinary string deltas.
pub struct ChatHandle<D: ChatDelivery> {
    inner: ExecutionHandle,
    delivery: PhantomData<fn() -> D>,
}

impl<D: ChatDelivery> ChatHandle<D> {
    pub(crate) fn new(inner: ExecutionHandle) -> Self {
        Self {
            inner,
            delivery: PhantomData,
        }
    }

    /// Receive the next event allowed by this delivery mode.
    pub async fn recv(&mut self) -> Option<TurnEvent<D::Delta>> {
        while let Some(event) = self.inner.recv().await {
            if let Some(event) = event.try_map_deltas(D::map_delta) {
                return Some(event);
            }
        }
        None
    }

    /// Get a clone of the pause token for execution registries.
    pub fn pause_token(&self) -> PauseToken {
        self.inner.pause_token()
    }

    /// Get the input sender used to enqueue follow-up user messages.
    pub fn turn_input(&self) -> TurnInputSender {
        self.inner.turn_input()
    }

    /// Request cooperative cancellation.
    pub fn cancel(&self) {
        self.inner.cancel();
    }

    /// Abort the execution after requesting cooperative cancellation.
    pub fn abort(&self) {
        self.inner.abort();
    }

    /// Pause before the next model request.
    pub fn pause(&self) {
        self.inner.pause();
    }

    /// Resume a paused execution.
    pub fn resume(&self) {
        self.inner.resume();
    }

    /// Whether this execution is currently paused.
    pub fn is_paused(&self) -> bool {
        self.inner.is_paused()
    }

    /// Wait for the complete turn output.
    pub async fn output(self) -> Result<TurnOutput> {
        self.inner.output().await
    }
}

/// Events returned by buffered chat handles.
pub type BufferedChatEvent = TurnEvent<BufferedDelta>;

/// Events returned by streaming chat handles.
pub type StreamingChatEvent = TurnEvent<String>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffered_delivery_removes_only_delta_events() {
        let delta: TurnEvent<String> = TurnEvent::AssistantTextDelta {
            request_id: "request-1".to_string(),
            delta: "partial".to_string(),
        };
        let buffered: Option<TurnEvent<BufferedDelta>> = delta.try_map_deltas(Buffered::map_delta);
        assert!(buffered.is_none());

        let completed: TurnEvent<String> = TurnEvent::ModelRequestCompleted {
            request_id: "request-1".to_string(),
            parent_call_id: None,
        };
        let buffered: Option<TurnEvent<BufferedDelta>> =
            completed.try_map_deltas(Buffered::map_delta);
        assert!(matches!(
            buffered,
            Some(TurnEvent::ModelRequestCompleted { request_id, .. })
                if request_id == "request-1"
        ));
    }

    #[test]
    fn streaming_delivery_preserves_delta_events() {
        let delta: TurnEvent<String> = TurnEvent::AssistantReasoningDelta {
            request_id: "request-1".to_string(),
            delta: "partial".to_string(),
        };
        let streamed = delta.try_map_deltas(Streaming::map_delta);

        assert!(matches!(
            streamed,
            Some(TurnEvent::AssistantReasoningDelta { delta, .. }) if delta == "partial"
        ));
    }
}
