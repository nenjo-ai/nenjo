//! Durable, platform-independent task inbox and schedule coordination.
//!
//! The module keeps persisted domain types, store contracts, timezone-aware
//! schedule calculation, and runtime coordination separate so hosts can
//! replace persistence and execution adapters without inheriting transport
//! concerns.

mod runtime;
mod store;
mod types;

pub use runtime::TaskRuntime;
pub use store::{CancellationOutcome, EnqueueOutcome, OccurrenceOutcome, TaskRuntimeStore};
pub use types::{
    TaskContent, TaskExecutionState, TaskExecutionTarget, TaskExecutorOutcome, TaskInboxItem,
    TaskRuntimeEvent, TaskSchedule, TaskSubmission, TaskTrigger,
};

#[cfg(test)]
mod tests;
