//! # nenjo
//!
//! The Nenjo agent platform SDK.
//!
//! This crate owns the agent turn loop, prompt building, type definitions
//! (domains, abilities, bootstrap types), and the execution handle API.
//!
//! ```ignore
//! use nenjo::{AgentRun, ChatInput};
//!
//! let agent = provider.agent_by_name("my-coder").await?.build().await?;
//! let handle = agent
//!     .run_stream(AgentRun::chat(ChatInput::new("Hello!").project_id(project_id)))
//!     .await?;
//! let output = handle.output().await?;
//! println!("{}", output.text);
//! ```

pub mod agents;
pub mod client;
pub mod config;
pub mod context;
pub mod input;
pub mod manifest;
pub mod memory;
pub mod provider;
pub mod repo_manifest;
pub mod routines;
pub mod tools;
pub mod types;

// Re-export key types at the crate root.
pub use agents::{AgentBuilder, AgentError, AgentInstance, AgentRunner};
pub use agents::{ExecutionHandle, TurnEvent, TurnLoopConfig, TurnOutput};
pub use config::AgentConfig;
pub use input::{
    AgentRun, AgentRunKind, ChatInput, CronInput, ExecutionOptions, GateInput, HeartbeatInput,
    ProjectLocation, RoutineRun, RoutineRunKind, TaskInput,
};
pub use manifest::{
    Manifest, ManifestLoader, ManifestResource, ManifestResourceKind,
    local::LocalManifestStore,
    store::{ManifestReader, ManifestWriter},
};
pub use provider::{
    ErasedProvider, ModelProviderFactory, Provider, ProviderBuilder, ProviderError,
    ProviderRuntime, RoutineRunner, ToolContext, ToolFactory, TypedModelProviderFactory,
};

// Re-export the Tool API for custom tool implementations.
pub use tools::{Tool, ToolAutonomy, ToolCategory, ToolResult, ToolSecurity, ToolSpec};

// Re-export the model provider trait for custom model implementations.
pub use nenjo_models::ModelProvider;

// Re-export StreamEvent for streaming consumers (Nenjo platform events).
pub use nenjo_events::StreamEvent;

// Re-export the XML/template crate for downstream consumers.
pub use nenjo_xml as xml;

// Re-export the API client.
pub use client::{ApiClientError, NenjoClient};

// Re-export routine types.
pub use routines::{RoutineEvent, RoutineExecutionHandle, RoutineInput, StepResult};
