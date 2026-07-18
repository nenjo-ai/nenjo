//! Harness for running Nenjo agents.
//!
//! `nenjo-harness` wraps an assembled [`Provider`] with session, transcript,
//! trace, and task runtime services. It is intentionally
//! usable without the Nenjo platform: hosts provide runtime integrations through
//! [`HarnessBuilder`], and omitted integrations use no-op defaults.
//!
//! The common path is request-oriented:
//!
//! ```ignore
//! let output = harness
//!     .chat(ChatRequest::new("coder", "Fix the failing test")
//!         .with_session(session_id))
//!     .await?;
//!
//! let mut stream = harness
//!     .task_stream(TaskRequest::new("website", "Title", "Description")
//!         .with_task_id(task_id)
//!         .with_routine("daily_maintenance"))
//!     .await?;
//! ```
//!
//! Platform adapters live in worker crates. The core harness API stays focused
//! on provider-backed execution.
//!
//! # Assembly
//!
//! ```ignore
//! use nenjo_harness::{Harness, Provider};
//!
//! # async fn example(provider: Provider) {
//! let harness = Harness::builder(provider)
//!     .with_session_runtime(nenjo_sessions::NoopSessionRuntime)
//!     .build();
//!
//! let provider_snapshot = harness.provider();
//! # let _ = provider_snapshot;
//! # }
//! ```

use std::sync::Arc;

use arc_swap::ArcSwap;
use nenjo_sessions::SessionRuntime;

use crate::domain::DomainRegistry;
pub use crate::error::{HarnessError, Result};
use crate::manifest::HarnessManifests;
use crate::registry::ExecutionRegistry;
use crate::state::HarnessInner;
pub use builder::HarnessBuilder;
pub use nenjo::provider::NoopToolFactory;
pub use nenjo::{
    ErasedProvider, Manifest, ManifestLoader, ModelProvider, ModelProviderFactory, Provider,
    ProviderBuilder, ProviderError, ProviderRuntime, ToolContext, ToolFactory,
    TypedModelProviderFactory,
};

pub mod builder;
pub mod domain;
pub mod error;
pub mod events;
pub(crate) mod execution_context;
pub mod handle;
#[cfg(feature = "local-runtime")]
pub mod local_runtime;
pub mod manifest;
pub mod preview;
pub mod registry;
pub mod request;
pub(crate) mod run;
pub mod session;
pub(crate) mod state;
pub mod task_runtime;
pub mod task_session;

pub mod prelude {
    pub use crate::{
        ChatRequest, Harness, HarnessBuilder, HarnessError, HarnessEvent, HarnessExecutionHandle,
        HarnessSessions, Manifest, ModelProviderFactory, Provider, ProviderBuilder,
        ProviderRuntime, Result, TaskRequest, ToolFactory, TypedModelProviderFactory,
    };
    pub use nenjo_sessions::{
        CheckpointRecord, SessionRuntimeEvent, SessionTranscriptRecord, SessionUpsert,
    };
}

pub use events::HarnessEvent;
pub use handle::HarnessExecutionHandle;
#[cfg(feature = "local-runtime")]
pub use local_runtime::{
    FileCheckpointStore, FileSessionRuntime, FileSessionStore, FileSessionStores, FileTraceStore,
    FileTranscriptStore, SessionRecoveryHandler,
};
pub use request::{ChatDomainActivation, ChatRequest, TaskRequest};
pub use session::HarnessSessions;
pub use task_runtime::{
    CancellationOutcome, EnqueueOutcome, TaskContent, TaskExecutionState, TaskExecutionTarget,
    TaskExecutorOutcome, TaskInboxItem, TaskRuntime, TaskRuntimeEvent, TaskRuntimeStore,
    TaskSchedule, TaskSubmission, TaskTrigger,
};

/// Cloneable, thread-safe harness handle.
///
/// The harness is intentionally a shallow `Arc` handle so hosts can clone it
/// into spawned execution tasks while shared state stays behind thread-safe
/// primitives.
pub struct Harness<
    P: ProviderRuntime = nenjo::provider::ErasedProvider,
    SessionRt: SessionRuntime = nenjo_sessions::NoopSessionRuntime,
> {
    pub(crate) inner: Arc<HarnessInner<P, SessionRt>>,
}

impl<P> Harness<P>
where
    P: ProviderRuntime,
{
    /// Start configuring a harness around an assembled provider.
    pub fn builder(provider: P) -> HarnessBuilder<P> {
        HarnessBuilder::new(provider)
    }
}

impl<P, SessionRt> Harness<P, SessionRt>
where
    P: ProviderRuntime,
    SessionRt: SessionRuntime + 'static,
{
    /// Send a chat message through the harness and wait for the final output.
    pub async fn chat(&self, request: ChatRequest) -> Result<nenjo::TurnOutput> {
        self.chat_stream(request).await?.output().await
    }

    /// Send a chat message through the harness and stream harness-native events.
    pub async fn chat_stream(&self, request: ChatRequest) -> Result<HarnessExecutionHandle> {
        crate::run::chat::chat_stream(self, request).await
    }

    /// Queue a user message into an active chat turn for the same session.
    pub async fn try_enqueue_chat_message(&self, request: &ChatRequest) -> Result<bool> {
        crate::run::chat::try_enqueue_chat_message(self, request).await
    }

    /// Execute a task through the harness and wait for the final output.
    pub async fn task(&self, request: TaskRequest) -> Result<nenjo::TurnOutput> {
        self.task_stream(request).await?.output().await
    }

    /// Execute a task through the harness and stream harness-native events.
    pub async fn task_stream(&self, request: TaskRequest) -> Result<HarnessExecutionHandle> {
        crate::run::task::task_stream(self, request).await
    }

    /// Load the current provider snapshot.
    pub fn provider(&self) -> Arc<P> {
        self.inner.provider.load_full()
    }

    /// Clone the live provider cell for long-running tasks.
    pub fn provider_handle(&self) -> Arc<ArcSwap<P>> {
        self.inner.provider.clone()
    }

    /// Active execution registry shared by host integrations.
    pub fn executions(&self) -> ExecutionRegistry {
        self.inner.executions.clone()
    }

    /// Active domain session registry shared by host integrations.
    pub fn domains(&self) -> DomainRegistry<P> {
        self.inner.domains.clone()
    }

    /// Session runtime services configured by the host.
    pub fn sessions(&self) -> HarnessSessions<SessionRt> {
        HarnessSessions::new(
            self.inner.session_runtime.clone(),
            self.inner.session_event_writer.clone(),
        )
    }

    /// Manifest services for inspecting or replacing the running provider manifest.
    pub fn manifests(&self) -> HarnessManifests<P, SessionRt> {
        HarnessManifests::new(self.clone())
    }

    /// Replace the current provider snapshot.
    pub fn swap_provider(&self, provider: P) {
        self.inner.provider.store(Arc::new(provider));
    }
}

impl<P, SessionRt> Clone for Harness<P, SessionRt>
where
    P: ProviderRuntime,
    SessionRt: SessionRuntime,
{
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::{Manifest, ModelProviderFactory, NoopToolFactory, Provider};

    use super::Harness;

    struct TestModelProvider;

    #[async_trait::async_trait]
    impl nenjo::ModelProvider for TestModelProvider {
        async fn chat(
            &self,
            _request: nenjo_models::ChatRequest<'_>,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<nenjo_models::ChatResponse> {
            Ok(nenjo_models::ChatResponse {
                text: Some("ok".to_string()),
                tool_calls: vec![],
                provider_tool_calls: vec![],
                usage: nenjo_models::TokenUsage::default(),
            })
        }
    }

    struct TestModelFactory;

    impl ModelProviderFactory for TestModelFactory {
        fn create(&self, _provider_name: &str) -> anyhow::Result<Arc<dyn nenjo::ModelProvider>> {
            Ok(Arc::new(TestModelProvider))
        }
    }

    async fn test_provider()
    -> Provider<TestModelFactory, NoopToolFactory, nenjo::provider::builder::NoMemory> {
        Provider::builder()
            .with_manifest(Manifest::default())
            .with_model_factory(TestModelFactory)
            .with_tool_factory(NoopToolFactory)
            .build()
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn harness_exposes_provider_and_session_runtime() {
        let provider = test_provider().await;
        let harness = Harness::builder(provider).build();
        let session_id = uuid::Uuid::new_v4();

        assert!(harness.provider().manifest_snapshot().agents.is_empty());
        harness
            .sessions()
            .record(nenjo_sessions::SessionRuntimeEvent::Checkpoint(
                nenjo_sessions::CheckpointRecord {
                    session_id,
                    turn_id: None,
                    checkpoint: nenjo_sessions::SessionCheckpoint {
                        session_id,
                        seq: 1,
                        saved_at: chrono::Utc::now(),
                        current_phase: None,
                        active_tool_name: None,
                        worktree: None,
                    },
                },
            ))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn harness_swaps_provider() {
        let provider = test_provider().await;
        let harness = Harness::builder(provider).build();
        let replacement = test_provider().await;

        harness.swap_provider(replacement);
        assert!(harness.provider().manifest_snapshot().agents.is_empty());
    }

    fn assert_send_sync<T: Send + Sync>() {}
    fn assert_clone<T: Clone>() {}

    #[test]
    fn harness_is_send_sync_and_clone() {
        assert_send_sync::<super::Harness>();
        assert_clone::<super::Harness>();
    }
}
