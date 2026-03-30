//! # nenjo
//!
//! The Nenjo agent platform SDK.
//!
//! This crate owns the agent turn loop, prompt building, type definitions
//! (domains, abilities, bootstrap types), and the execution handle API.
//!
//! ```ignore
//! use nenjo::{Nenjo, Chat};
//!
//! let nenjo = Nenjo::builder()
//!     .provider(my_provider)
//!     .build()?;
//!
//! let agent = nenjo.agent("my-coder")?;
//! let handle = agent.chat(Chat::builder().message("Hello!").build()).await?;
//! let output = handle.output().await?;
//! println!("{}", output.text);
//! ```

pub mod agents;
pub mod client;
pub mod config;
pub mod context;
pub mod manifest;
pub mod memory;
pub mod provider;
pub mod routines;
pub mod types;

// Re-export key types at the crate root.
pub use agents::{AgentBuilder, AgentError, AgentInstance, AgentRunner};
pub use agents::{ExecutionHandle, TurnEvent, TurnLoopConfig, TurnOutput};
pub use config::AgentConfig;
pub use manifest::{Manifest, ManifestLoader};
pub use provider::{
    ModelProviderFactory, Provider, ProviderBuilder, ProviderError, RoutineRunner, ToolFactory,
};

// Re-export the Tool trait for custom tool implementations.
pub use nenjo_tools::{Tool, ToolCategory, ToolResult, ToolSpec};

// Re-export Provider for convenience.
pub use nenjo_models::ModelProvider;

// Re-export StreamEvent for streaming consumers (Nenjo platform events).
pub use nenjo_events::StreamEvent;

// Re-export the XML/template crate for downstream consumers.
pub use nenjo_xml as xml;

// Re-export the API client.
pub use client::{ApiClientError, NenjoClient};

// Re-export routine types.
pub use routines::{
    LambdaOutput, LambdaRunner, RoutineEvent, RoutineExecutionHandle, RoutineInput, StepResult,
};
