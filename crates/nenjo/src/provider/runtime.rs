use std::sync::Arc;

use anyhow::Result;
use uuid::Uuid;

use super::{ProviderError, ToolFactory, builder};
use crate::agents::builder::AgentBuilder;
use crate::agents::prompts::PromptContext;
use crate::manifest::{AgentManifest, Manifest, ModelManifest, ProjectManifest};
use crate::memory::Memory;
use crate::tools::Tool;

#[doc(hidden)]
pub trait ProviderMemory: Send + Sync + 'static {
    type Runtime<'a>: Memory + ?Sized + 'a
    where
        Self: 'a;

    fn clone_runtime(memory: Option<&Arc<Self>>) -> Option<Arc<Self::Runtime<'static>>>;
}

impl<T> ProviderMemory for T
where
    T: Memory + Sized + 'static,
{
    type Runtime<'a>
        = T
    where
        Self: 'a;

    fn clone_runtime(memory: Option<&Arc<Self>>) -> Option<Arc<Self::Runtime<'static>>> {
        memory.cloned()
    }
}

impl ProviderMemory for dyn Memory + 'static {
    type Runtime<'a>
        = dyn Memory + 'static
    where
        Self: 'a;

    fn clone_runtime(memory: Option<&Arc<Self>>) -> Option<Arc<Self::Runtime<'static>>> {
        memory.cloned()
    }
}

impl ProviderMemory for builder::NoMemory {
    type Runtime<'a>
        = dyn Memory + 'static
    where
        Self: 'a;

    fn clone_runtime(_memory: Option<&Arc<Self>>) -> Option<Arc<Self::Runtime<'static>>> {
        None
    }
}

/// Runtime contract required by generic agent and routine execution.
///
/// `ProviderRuntime` lets runners stay generic over the concrete provider,
/// model factory, tool factory, and memory backend. The concrete
/// [`Provider`](crate::provider::Provider) implements this trait, and most
/// applications can use `Provider` directly. Implement this trait only when you
/// need a custom provider-like runtime for tests or embedding.
///
/// The naming convention is intentional:
///
/// - `find_*` methods borrow data already present in the manifest.
/// - `create_*` methods construct runtime dependencies.
/// - `build_*` methods return higher-level builders.
/// - [`new_agent`](Self::new_agent) starts a blank agent builder that may be
///   configured without an agent already present in the provider manifest.
#[async_trait::async_trait]
pub trait ProviderRuntime: Clone + Send + Sync + 'static {
    /// Model provider type created for an agent model manifest.
    type Model<'a>: nenjo_models::ModelProvider + Send + Sync + ?Sized + 'a
    where
        Self: 'a;

    /// Tool factory type used when constructing an agent's runtime tools.
    type ToolFactory<'a>: ToolFactory + ?Sized + 'a
    where
        Self: 'a;

    /// Memory backend type available to agent builders.
    type Memory<'a>: Memory + ?Sized + 'a
    where
        Self: 'a;

    /// Return an owned snapshot of the manifest used by this runtime.
    fn manifest_snapshot(&self) -> Arc<Manifest>;

    /// Borrow the runtime's tool factory.
    fn tool_factory(&self) -> &Self::ToolFactory<'_>;

    /// Find an agent manifest by name.
    fn find_agent_manifest(&self, name: &str) -> Option<&AgentManifest>;

    /// Find a project manifest by ID.
    fn find_project(&self, id: Uuid) -> Option<&ProjectManifest>;

    /// Create provider-level knowledge tools.
    fn create_knowledge_tools(&self) -> Vec<Arc<dyn Tool>>;

    /// Build prompt context for an agent manifest.
    fn build_prompt_context(&self, agent: &AgentManifest) -> PromptContext;

    /// Create the model provider requested by a model manifest.
    async fn create_model_provider(
        &self,
        model: &ModelManifest,
    ) -> Result<Arc<Self::Model<'static>>, ProviderError>;

    /// Start a blank agent builder backed by this provider runtime.
    fn new_agent(&self) -> AgentBuilder<Self>;

    /// Build an agent from the provider manifest by ID.
    async fn build_agent_by_id(&self, id: Uuid) -> Result<AgentBuilder<Self>, ProviderError>;

    /// Build an agent from the provider manifest by name.
    async fn build_agent_by_name(&self, name: &str) -> Result<AgentBuilder<Self>, ProviderError>;
}
