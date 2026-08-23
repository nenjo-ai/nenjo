//! Async handles returned by harness execution APIs.

use std::marker::PhantomData;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::events::{HarnessChatEvent, HarnessEvent};

/// Handle returned by harness streaming execution APIs.
pub struct HarnessExecutionHandle {
    events_rx: mpsc::UnboundedReceiver<HarnessEvent>,
    join: JoinHandle<crate::Result<nenjo::TurnOutput>>,
    cancel: CancellationToken,
}

impl HarnessExecutionHandle {
    pub(crate) fn new(
        events_rx: mpsc::UnboundedReceiver<HarnessEvent>,
        join: JoinHandle<crate::Result<nenjo::TurnOutput>>,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            events_rx,
            join,
            cancel,
        }
    }

    /// Receive the next harness event.
    pub async fn recv(&mut self) -> Option<HarnessEvent> {
        self.events_rx.recv().await
    }

    /// Cancel the running execution.
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    /// Wait for the final output.
    pub async fn output(self) -> crate::Result<nenjo::TurnOutput> {
        self.join.await.map_err(anyhow::Error::from)?
    }
}

/// Typed handle returned by [`Harness::chat`](crate::Harness::chat).
pub struct HarnessChatHandle<D: nenjo::ChatDelivery> {
    inner: HarnessExecutionHandle,
    delivery: PhantomData<fn() -> D>,
}

impl<D: nenjo::ChatDelivery> HarnessChatHandle<D> {
    pub(crate) fn new(inner: HarnessExecutionHandle) -> Self {
        Self {
            inner,
            delivery: PhantomData,
        }
    }

    /// Receive the next chat event allowed by this delivery mode.
    pub async fn recv(&mut self) -> Option<HarnessChatEvent<D::Delta>> {
        loop {
            match self.inner.recv().await? {
                HarnessEvent::DomainEntered {
                    session_id,
                    domain_name,
                } => {
                    return Some(HarnessChatEvent::DomainEntered {
                        session_id,
                        domain_name,
                    });
                }
                HarnessEvent::Turn {
                    session_id,
                    turn_id,
                    event,
                } => {
                    if let Some(event) = event.try_map_deltas(D::map_delta) {
                        return Some(HarnessChatEvent::Turn {
                            session_id,
                            turn_id,
                            event,
                        });
                    }
                }
                HarnessEvent::Routine { .. } => {}
            }
        }
    }

    /// Cancel the running chat execution.
    pub fn cancel(&self) {
        self.inner.cancel();
    }

    /// Wait for the complete chat output.
    pub async fn output(self) -> crate::Result<nenjo::TurnOutput> {
        self.inner.output().await
    }
}
