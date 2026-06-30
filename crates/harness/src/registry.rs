//! Active execution registry types.

use std::sync::Arc;

use dashmap::DashMap;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// What kind of execution this is for targeted cancellation and lifecycle work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionKind {
    Chat,
    PreparingTask,
    Task,
    Cron,
    Heartbeat,
}

/// Tracks an active execution so it can be cancelled or paused.
pub struct ActiveExecution {
    pub kind: ExecutionKind,
    pub registry_token: Uuid,
    pub execution_run_id: Option<Uuid>,
    pub cancel: CancellationToken,
    pub pause: Option<nenjo::agents::runner::types::PauseToken>,
    pub turn_input: Option<nenjo::agents::runner::types::TurnInputSender>,
}

/// Thread-safe registry of active executions, keyed by a cancel key.
pub type ExecutionRegistry = Arc<DashMap<Uuid, ActiveExecution>>;
