//! Async handles returned by harness execution APIs.

use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::events::HarnessEvent;

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
