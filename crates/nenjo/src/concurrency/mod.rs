//! Bounded resource admission and execution identity for nested agent work.
//!
//! [`AdmissionPool`](crate::concurrency::AdmissionPool) limits physical resources
//! and root executions. Its fair queue is independent of
//! [`ExecutionContext`](crate::concurrency::ExecutionContext), which supplies root
//! attribution and shares descendant permits only while an execution is runnable.
//!
//! Resource permits belong to the operation using the resource. Runnable leases
//! belong to a phase, so waiting parents do not occupy their descendants' slots.

mod admission;
mod execution;

pub use admission::{AdmissionError, AdmissionPermit, AdmissionPool};
pub use execution::{ExecutionContext, current_root_id};
pub(crate) use execution::{in_scope, runnable};
