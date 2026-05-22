//! Async handles returned by harness execution APIs.

use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::events::{HarnessEvent, HarnessScheduleEvent};

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

/// Handle returned by scheduled harness APIs.
pub struct HarnessScheduleHandle {
    events_rx: mpsc::UnboundedReceiver<HarnessScheduleEvent>,
    join: Option<JoinHandle<crate::Result<()>>>,
    cancel: CancellationToken,
}

impl HarnessScheduleHandle {
    pub(crate) fn new(
        events_rx: mpsc::UnboundedReceiver<HarnessScheduleEvent>,
        join: JoinHandle<crate::Result<()>>,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            events_rx,
            join: Some(join),
            cancel,
        }
    }

    /// Receive the next scheduler event.
    pub async fn recv(&mut self) -> Option<HarnessScheduleEvent> {
        self.events_rx.recv().await
    }

    /// Stop the schedule.
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    /// Wait for the schedule task to stop.
    pub async fn stopped(mut self) -> crate::Result<()> {
        let Some(join) = self.join.take() else {
            return Ok(());
        };
        join.await.map_err(anyhow::Error::from)?
    }
}

impl Drop for HarnessScheduleHandle {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
