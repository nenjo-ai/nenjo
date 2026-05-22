//! Harness for running Nenjo agents.
//!
//! `nenjo-harness` wraps an assembled [`Provider`] with session,
//! transcript, trace, and scheduling services. It is intentionally
//! usable without the Nenjo platform: hosts provide runtime integrations through
//! [`HarnessBuilder`], and omitted integrations use no-op defaults.
//!
//! The common path is request-oriented:
//!
//! ```ignore
//! let output = harness
//!     .chat(ChatRequest::new(session_id, "coder", "Fix the failing test"))
//!     .await?;
//!
//! let mut stream = harness
//!     .task_stream(TaskRequest::new(task_id, project_id, "Title", "Description"))
//!     .await?;
//!
//! let mut schedule = harness
//!     .heartbeat(HeartbeatRequest::new(agent_id, std::time::Duration::from_secs(300)))
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

pub mod prelude {
    pub use crate::{
        AgentRef, ChatRequest, CronRequest, Harness, HarnessBuilder, HarnessError, HarnessEvent,
        HarnessExecutionHandle, HarnessScheduleEvent, HarnessScheduleHandle, HarnessSessions,
        HeartbeatRequest, Manifest, ModelProviderFactory, Provider, ProviderBuilder,
        ProviderRuntime, Result, TaskRequest, ToolFactory, TypedModelProviderFactory,
    };
    pub use nenjo_sessions::{
        CheckpointRecord, SessionRuntimeEvent, SessionTranscriptRecord, SessionUpsert,
    };
}

pub use events::{HarnessEvent, HarnessScheduleEvent};
pub use handle::{HarnessExecutionHandle, HarnessScheduleHandle};
#[cfg(feature = "local-runtime")]
pub use local_runtime::{
    FileCheckpointStore, FileSessionRecoveryHandler, FileSessionRuntime, FileSessionStore,
    FileSessionStores, FileTraceStore, FileTranscriptStore, LocalSessionCoordinator,
};
pub use request::{
    AgentRef, ChatDomainActivation, ChatRequest, CronRequest, HeartbeatRequest, TaskRequest,
};
pub use session::HarnessSessions;

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

    /// Execute a task through the harness and wait for the final output.
    pub async fn task(&self, request: TaskRequest) -> Result<nenjo::TurnOutput> {
        self.task_stream(request).await?.output().await
    }

    /// Execute a task through the harness and stream harness-native events.
    pub async fn task_stream(&self, request: TaskRequest) -> Result<HarnessExecutionHandle> {
        crate::run::task::task_stream(self, request).await
    }

    /// Schedule a cron routine through the harness.
    pub async fn cron(&self, request: CronRequest) -> Result<HarnessScheduleHandle> {
        crate::run::cron::cron(self, request).await
    }

    /// Schedule an agent heartbeat through the harness.
    pub async fn heartbeat(&self, request: HeartbeatRequest) -> Result<HarnessScheduleHandle> {
        crate::run::heartbeat::heartbeat(self, request).await
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
                        scheduler_runtime: None,
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
