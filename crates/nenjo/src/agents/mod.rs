//! Agent building blocks: instance, builder, runner, prompts.

pub mod abilities;
pub(crate) mod async_ops;
pub mod builder;
pub(crate) mod delegation;
pub mod error;
pub mod instance;
pub mod prompts;
pub(crate) mod respond;
pub mod runner;
pub(crate) mod sub_agents;

pub use async_ops::{
    AsyncOperationHandle, AsyncOperationRuntime, StartAsyncOperation,
    current_async_operation_runtime,
};
pub use builder::AgentBuilder;
pub use error::AgentError;
pub(crate) use instance::AgentExecutionMode;
pub use instance::AgentInstance;
pub use runner::types::{
    AsyncOperationTranscriptEvent, SubAgentTranscriptEvent, ToolCall, TurnEvent, TurnLoopConfig,
    TurnOutput,
};
pub use runner::{AgentRunner, ExecutionHandle};
