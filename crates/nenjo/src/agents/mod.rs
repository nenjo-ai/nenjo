//! Agent building blocks: instance, builder, runner, prompts.

pub mod abilities;
pub mod builder;
pub mod error;
pub mod instance;
pub mod prompts;
pub mod runner;
pub(crate) mod sub_agents;

pub use builder::AgentBuilder;
pub use error::AgentError;
pub(crate) use instance::AgentExecutionMode;
pub use instance::AgentInstance;
pub use runner::types::{SubAgentTranscriptEvent, ToolCall, TurnEvent, TurnLoopConfig, TurnOutput};
pub use runner::{AgentRunner, ExecutionHandle};
