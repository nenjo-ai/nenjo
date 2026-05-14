//! Nenjo platform harness runtime.
//!
//! This crate owns the platform harness boundary around an assembled typed
//! [`nenjo::Provider`]: provider access, session runtime hooks, platform event
//! bridging, preview formatting, manifest updates, and execution trace state.
//! Worker crates remain responsible for concrete event buses, auth, tool
//! factories, persistence implementations, and process lifecycle.
//!
//! Runtime integrations are generic. A host can keep concrete types for session
//! persistence, execution traces, manifest storage, and MCP reconciliation by
//! installing them through [`HarnessBuilder`]. Omitted integrations fall back to
//! no-op runtimes.
//!
//! The [`prelude`] module is intentionally small: it re-exports the embedded
//! harness context and session event types. Platform-facing adapters for
//! event conversion, preview text, and trace recording remain available through
//! the explicit [`event_bridge`], [`preview`], and [`trace`] modules.
//!
//! # Platform Harness Assembly
//!
//! ```ignore
//! use nenjo_harness::Harness;
//!
//! # async fn example(provider: nenjo::Provider) {
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
use dashmap::DashMap;
use nenjo::manifest::Manifest;
use nenjo::provider::{ErasedProvider, ProviderMemory, ProviderRuntime, ToolFactory};
use nenjo::{Provider, TypedModelProviderFactory};
use nenjo_sessions::{
    ChatSessionUpsert, CheckpointQuery, DomainSessionUpsert, SchedulerSessionUpsert,
    SessionCheckpoint, SessionCheckpointUpdate, SessionRecord, SessionRuntime, SessionRuntimeEvent,
    SessionTranscriptAppend, SessionTranscriptEvent, SessionTransition, TaskSessionUpsert,
    TranscriptQuery,
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub use crate::error::{HarnessError, Result};
use crate::execution_trace::{ExecutionTraceRuntime, NoopExecutionTraceRuntime};
use crate::handlers::manifest::{
    ManifestServices, ManifestStore, McpRuntime, NoopManifestStore, NoopMcpRuntime,
};

pub mod error;
pub mod event_bridge;
pub mod execution_trace;
pub mod handlers;
pub mod preview;
pub mod session;
pub mod trace;

pub mod prelude {
    pub use crate::{
        ActiveExecution, DomainRegistry, DomainSession, ExecutionKind, ExecutionRegistry, GitLocks,
        Harness, HarnessBuilder, HarnessError, Result,
    };
    pub use nenjo_sessions::{
        CheckpointRecord, SessionRuntimeEvent, SessionTranscriptRecord, SessionUpsert,
    };
}

/// Provider capabilities required by the platform harness.
///
/// Agent and routine execution use [`ProviderRuntime`]. The harness also needs
/// borrowed manifest access for routing decisions and a way to rebuild a
/// provider after manifest updates arrive from the platform.
pub trait HarnessProvider: ProviderRuntime {
    /// Borrow the current bootstrap manifest.
    fn manifest(&self) -> &Manifest;

    /// Return a provider with the same runtime services and a new manifest.
    fn with_manifest(&self, manifest: Manifest) -> Self;

    /// Build a routine runner for a routine in the current manifest.
    fn routine_by_id(
        &self,
        routine_id: Uuid,
    ) -> std::result::Result<nenjo::RoutineRunner<Self>, nenjo::ProviderError>
    where
        Self: Sized;
}

impl<ModelFactory, ToolFactoryImpl, Mem> HarnessProvider
    for Provider<ModelFactory, ToolFactoryImpl, Mem>
where
    ModelFactory: TypedModelProviderFactory + ?Sized + 'static,
    ToolFactoryImpl: ToolFactory + ?Sized + 'static,
    Mem: ProviderMemory + ?Sized + 'static,
{
    fn manifest(&self) -> &Manifest {
        Provider::manifest(self)
    }

    fn with_manifest(&self, manifest: Manifest) -> Self {
        Provider::with_manifest(self, manifest)
    }

    fn routine_by_id(
        &self,
        routine_id: Uuid,
    ) -> std::result::Result<nenjo::RoutineRunner<Self>, nenjo::ProviderError> {
        Provider::routine_by_id(self, routine_id)
    }
}

/// What kind of execution this is for targeted cancellation and lifecycle work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionKind {
    Chat,
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
}

/// Thread-safe registry of active executions, keyed by a cancel key.
pub type ExecutionRegistry = Arc<DashMap<Uuid, ActiveExecution>>;

/// An active domain session holding the domain-expanded runner and state.
pub struct DomainSession<P: HarnessProvider = ErasedProvider> {
    pub runner: nenjo::AgentRunner<P>,
    pub agent_id: Uuid,
    pub project_id: Uuid,
    pub domain_command: String,
}

/// Thread-safe registry of active domain sessions, keyed by domain session id.
pub type DomainRegistry<P = ErasedProvider> = Arc<DashMap<Uuid, DomainSession<P>>>;

/// Per-repo mutexes used to serialize git worktree operations.
pub type GitLocks = Arc<DashMap<std::path::PathBuf, Arc<tokio::sync::Mutex<()>>>>;

/// Per-session mutexes used to preserve runtime event ordering in detached writers.
pub type SessionEventLocks = Arc<DashMap<Uuid, Arc<tokio::sync::Mutex<()>>>>;

/// Cloneable, thread-safe platform harness handle.
///
/// The harness is intentionally a shallow `Arc` handle so worker transports can
/// clone it into spawned command tasks. Handler methods should take `&self` and
/// store shared mutable state behind thread-safe primitives.
pub struct Harness<
    P: HarnessProvider = ErasedProvider,
    SessionRt: SessionRuntime = nenjo_sessions::NoopSessionRuntime,
    TraceRt: ExecutionTraceRuntime = NoopExecutionTraceRuntime,
    StoreRt: ManifestStore = NoopManifestStore,
    McpRt: McpRuntime = NoopMcpRuntime,
> {
    inner: Arc<HarnessInner<P, SessionRt, TraceRt, StoreRt, McpRt>>,
}

impl<P, SessionRt, TraceRt, StoreRt, McpRt> Clone for Harness<P, SessionRt, TraceRt, StoreRt, McpRt>
where
    P: HarnessProvider,
    SessionRt: SessionRuntime,
    TraceRt: ExecutionTraceRuntime,
    StoreRt: ManifestStore,
    McpRt: McpRuntime,
{
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

struct HarnessInner<
    P: HarnessProvider = ErasedProvider,
    SessionRt: SessionRuntime = nenjo_sessions::NoopSessionRuntime,
    TraceRt: ExecutionTraceRuntime = NoopExecutionTraceRuntime,
    StoreRt: ManifestStore = NoopManifestStore,
    McpRt: McpRuntime = NoopMcpRuntime,
> {
    provider: Arc<ArcSwap<P>>,
    session_runtime: Arc<SessionRt>,
    execution_trace_runtime: Arc<TraceRt>,
    manifest_services: Option<ManifestServices<StoreRt, McpRt>>,
    executions: ExecutionRegistry,
    domains: DomainRegistry<P>,
    git_locks: GitLocks,
    session_event_locks: SessionEventLocks,
}

impl<P> Harness<P>
where
    P: HarnessProvider,
{
    /// Start configuring a platform harness around an assembled provider.
    pub fn builder(provider: P) -> HarnessBuilder<P> {
        HarnessBuilder::new(provider)
    }
}

impl<P, SessionRt, TraceRt, StoreRt, McpRt> Harness<P, SessionRt, TraceRt, StoreRt, McpRt>
where
    P: HarnessProvider,
    SessionRt: SessionRuntime,
    TraceRt: ExecutionTraceRuntime,
    StoreRt: ManifestStore,
    McpRt: McpRuntime,
{
    /// Load the current provider snapshot.
    pub fn provider(&self) -> Arc<P> {
        self.inner.provider.load_full()
    }

    /// Clone the live provider cell for long-running platform tasks.
    pub fn provider_handle(&self) -> Arc<ArcSwap<P>> {
        self.inner.provider.clone()
    }

    /// Active execution registry shared by platform handlers.
    pub fn executions(&self) -> ExecutionRegistry {
        self.inner.executions.clone()
    }

    /// Active domain session registry shared by platform handlers.
    pub fn domains(&self) -> DomainRegistry<P> {
        self.inner.domains.clone()
    }

    /// Git operation locks shared by platform handlers.
    pub fn git_locks(&self) -> GitLocks {
        self.inner.git_locks.clone()
    }

    /// Session event locks shared by detached session runtime writers.
    pub fn session_event_locks(&self) -> SessionEventLocks {
        self.inner.session_event_locks.clone()
    }

    /// Execution trace runtime configured by the host.
    pub fn execution_traces(&self) -> Arc<TraceRt> {
        self.inner.execution_trace_runtime.clone()
    }

    /// Manifest services configured by the host, when manifest event handling is enabled.
    pub fn manifest_services(&self) -> Option<ManifestServices<StoreRt, McpRt>> {
        self.inner.manifest_services.clone()
    }

    /// Replace the current provider snapshot.
    pub fn swap_provider(&self, provider: P) {
        self.inner.provider.store(Arc::new(provider));
    }

    /// Record a single session event through the configured runtime.
    pub async fn record_session_event(&self, event: SessionRuntimeEvent) -> Result<()> {
        self.inner
            .session_runtime
            .record(event)
            .await
            .map_err(HarnessError::session_runtime)
    }

    /// Load a persisted session record.
    pub async fn get_session(&self, session_id: Uuid) -> Result<Option<SessionRecord>> {
        self.inner
            .session_runtime
            .get_session(session_id)
            .await
            .map_err(HarnessError::session_runtime)
    }

    /// List all persisted sessions known to the configured session runtime.
    pub async fn list_sessions(&self) -> Result<Vec<SessionRecord>> {
        self.inner
            .session_runtime
            .list_sessions()
            .await
            .map_err(HarnessError::session_runtime)
    }

    /// Delete a persisted session and its runtime-owned state.
    pub async fn delete_session(&self, session_id: Uuid) -> Result<()> {
        self.inner
            .session_runtime
            .delete_session(session_id)
            .await
            .map_err(HarnessError::session_runtime)
    }

    /// Read transcript events for a session.
    pub async fn read_transcript(
        &self,
        session_id: Uuid,
        query: TranscriptQuery,
    ) -> Result<Vec<SessionTranscriptEvent>> {
        self.inner
            .session_runtime
            .read_transcript(session_id, query)
            .await
            .map_err(HarnessError::session_runtime)
    }

    /// Append a transcript event through the configured session runtime.
    pub async fn append_transcript(
        &self,
        append: SessionTranscriptAppend,
    ) -> Result<Option<SessionTranscriptEvent>> {
        self.inner
            .session_runtime
            .append_transcript(append)
            .await
            .map_err(HarnessError::session_runtime)
    }

    /// Load the latest checkpoint for a session.
    pub async fn load_latest_checkpoint(
        &self,
        session_id: Uuid,
        query: CheckpointQuery,
    ) -> Result<Option<SessionCheckpoint>> {
        self.inner
            .session_runtime
            .load_latest_checkpoint(session_id, query)
            .await
            .map_err(HarnessError::session_runtime)
    }

    /// Update the checkpoint for a session.
    pub async fn update_session_checkpoint(&self, update: SessionCheckpointUpdate) -> Result<bool> {
        self.inner
            .session_runtime
            .update_checkpoint(update)
            .await
            .map_err(HarnessError::session_runtime)
    }

    /// Transition a session's lifecycle state.
    pub async fn transition_session(&self, transition: SessionTransition) -> Result<bool> {
        self.inner
            .session_runtime
            .transition_session(transition)
            .await
            .map_err(HarnessError::session_runtime)
    }

    /// Upsert scheduler state for cron and heartbeat sessions.
    pub async fn upsert_scheduler_session(&self, upsert: SchedulerSessionUpsert) -> Result<bool> {
        self.inner
            .session_runtime
            .upsert_scheduler_session(upsert)
            .await
            .map_err(HarnessError::session_runtime)
    }

    /// Upsert a chat session record.
    pub async fn upsert_chat_session(&self, upsert: ChatSessionUpsert) -> Result<bool> {
        self.inner
            .session_runtime
            .upsert_chat_session(upsert)
            .await
            .map_err(HarnessError::session_runtime)
    }

    /// Upsert a task session record.
    pub async fn upsert_task_session(&self, upsert: TaskSessionUpsert) -> Result<bool> {
        self.inner
            .session_runtime
            .upsert_task_session(upsert)
            .await
            .map_err(HarnessError::session_runtime)
    }

    /// Upsert a domain session record.
    pub async fn upsert_domain_session(&self, upsert: DomainSessionUpsert) -> Result<bool> {
        self.inner
            .session_runtime
            .upsert_domain_session(upsert)
            .await
            .map_err(HarnessError::session_runtime)
    }

    /// Resolve the memory namespace associated with a session, if any.
    pub async fn session_memory_namespace(&self, session_id: Uuid) -> Result<Option<String>> {
        self.inner
            .session_runtime
            .session_memory_namespace(session_id)
            .await
            .map_err(HarnessError::session_runtime)
    }
}

/// Builder for the cloneable [`Harness`] handle.
pub struct HarnessBuilder<
    P: HarnessProvider = ErasedProvider,
    SessionRt: SessionRuntime = nenjo_sessions::NoopSessionRuntime,
    TraceRt: ExecutionTraceRuntime = NoopExecutionTraceRuntime,
    StoreRt: ManifestStore = NoopManifestStore,
    McpRt: McpRuntime = NoopMcpRuntime,
> {
    provider: P,
    session_runtime: Arc<SessionRt>,
    execution_trace_runtime: Arc<TraceRt>,
    manifest_client: Option<Arc<nenjo::client::NenjoClient>>,
    manifest_store: Option<Arc<StoreRt>>,
    manifest_mcp: Option<Arc<McpRt>>,
    executions: Option<ExecutionRegistry>,
    domains: Option<DomainRegistry<P>>,
    git_locks: Option<GitLocks>,
    session_event_locks: Option<SessionEventLocks>,
}

impl<P> HarnessBuilder<P>
where
    P: HarnessProvider,
{
    /// Create a builder around an assembled provider.
    pub fn new(provider: P) -> Self {
        Self {
            provider,
            session_runtime: Arc::new(nenjo_sessions::NoopSessionRuntime),
            execution_trace_runtime: Arc::new(NoopExecutionTraceRuntime),
            manifest_client: None,
            manifest_store: None,
            manifest_mcp: None,
            executions: None,
            domains: None,
            git_locks: None,
            session_event_locks: None,
        }
    }
}

impl<P, SessionRt, TraceRt, StoreRt, McpRt> HarnessBuilder<P, SessionRt, TraceRt, StoreRt, McpRt>
where
    P: HarnessProvider,
    SessionRt: SessionRuntime,
    TraceRt: ExecutionTraceRuntime,
    StoreRt: ManifestStore,
    McpRt: McpRuntime,
{
    /// Use a concrete session runtime for upserts, evidence events, and checkpoints.
    pub fn with_session_runtime<NextSessionRt>(
        self,
        session_runtime: NextSessionRt,
    ) -> HarnessBuilder<P, NextSessionRt, TraceRt, StoreRt, McpRt>
    where
        NextSessionRt: SessionRuntime,
    {
        HarnessBuilder {
            provider: self.provider,
            session_runtime: Arc::new(session_runtime),
            execution_trace_runtime: self.execution_trace_runtime,
            manifest_client: self.manifest_client,
            manifest_store: self.manifest_store,
            manifest_mcp: self.manifest_mcp,
            executions: self.executions,
            domains: self.domains,
            git_locks: self.git_locks,
            session_event_locks: self.session_event_locks,
        }
    }

    /// Use a concrete trace runtime. Hosts normally provide storage-backed trace
    /// persistence here; embedded apps can omit it for no-op traces.
    pub fn with_execution_trace_runtime<NextTraceRt>(
        self,
        execution_trace_runtime: NextTraceRt,
    ) -> HarnessBuilder<P, SessionRt, NextTraceRt, StoreRt, McpRt>
    where
        NextTraceRt: ExecutionTraceRuntime,
    {
        HarnessBuilder {
            provider: self.provider,
            session_runtime: self.session_runtime,
            execution_trace_runtime: Arc::new(execution_trace_runtime),
            manifest_client: self.manifest_client,
            manifest_store: self.manifest_store,
            manifest_mcp: self.manifest_mcp,
            executions: self.executions,
            domains: self.domains,
            git_locks: self.git_locks,
            session_event_locks: self.session_event_locks,
        }
    }

    /// Use the platform client for fetching changed manifest resources.
    pub fn with_manifest_client<Client>(mut self, client: Client) -> Self
    where
        Client: Into<Arc<nenjo::client::NenjoClient>>,
    {
        self.manifest_client = Some(client.into());
        self
    }

    /// Use a concrete manifest store for persisting changed manifest resources.
    pub fn with_manifest_store<NextStoreRt>(
        self,
        store: NextStoreRt,
    ) -> HarnessBuilder<P, SessionRt, TraceRt, NextStoreRt, McpRt>
    where
        NextStoreRt: ManifestStore,
    {
        HarnessBuilder {
            provider: self.provider,
            session_runtime: self.session_runtime,
            execution_trace_runtime: self.execution_trace_runtime,
            manifest_client: self.manifest_client,
            manifest_store: Some(Arc::new(store)),
            manifest_mcp: self.manifest_mcp,
            executions: self.executions,
            domains: self.domains,
            git_locks: self.git_locks,
            session_event_locks: self.session_event_locks,
        }
    }

    /// Use a concrete MCP runtime for MCP server reconciliation.
    pub fn with_mcp_runtime<NextMcpRt>(
        self,
        mcp: NextMcpRt,
    ) -> HarnessBuilder<P, SessionRt, TraceRt, StoreRt, NextMcpRt>
    where
        NextMcpRt: McpRuntime,
    {
        HarnessBuilder {
            provider: self.provider,
            session_runtime: self.session_runtime,
            execution_trace_runtime: self.execution_trace_runtime,
            manifest_client: self.manifest_client,
            manifest_store: self.manifest_store,
            manifest_mcp: Some(Arc::new(mcp)),
            executions: self.executions,
            domains: self.domains,
            git_locks: self.git_locks,
            session_event_locks: self.session_event_locks,
        }
    }

    /// Use an existing execution registry. Hosts normally omit this and let the
    /// harness allocate one.
    pub fn with_execution_registry(mut self, executions: ExecutionRegistry) -> Self {
        self.executions = Some(executions);
        self
    }

    /// Use an existing domain-session registry. Hosts normally omit this and let
    /// the harness allocate one.
    pub fn with_domain_registry(mut self, domains: DomainRegistry<P>) -> Self {
        self.domains = Some(domains);
        self
    }

    /// Use an existing git lock registry. Hosts normally omit this and let the
    /// harness allocate one.
    pub fn with_git_locks(mut self, git_locks: GitLocks) -> Self {
        self.git_locks = Some(git_locks);
        self
    }

    /// Use an existing session event lock registry. Hosts normally omit this.
    pub fn with_session_event_locks(mut self, session_event_locks: SessionEventLocks) -> Self {
        self.session_event_locks = Some(session_event_locks);
        self
    }

    /// Build the cloneable platform harness.
    pub fn build(self) -> Harness<P, SessionRt, TraceRt, StoreRt, McpRt> {
        let manifest_services = match (self.manifest_client, self.manifest_store) {
            (Some(client), Some(store)) => Some(ManifestServices {
                client,
                store,
                mcp: self.manifest_mcp,
            }),
            (None, None) => {
                assert!(
                    self.manifest_mcp.is_none(),
                    "MCP services require with_manifest_client and with_manifest_store"
                );
                None
            }
            (Some(_), None) => {
                panic!("with_manifest_client requires with_manifest_store");
            }
            (None, Some(_)) => {
                panic!("with_manifest_store requires with_manifest_client");
            }
        };

        Harness {
            inner: Arc::new(HarnessInner {
                provider: Arc::new(ArcSwap::from_pointee(self.provider)),
                session_runtime: self.session_runtime,
                execution_trace_runtime: self.execution_trace_runtime,
                manifest_services,
                executions: self.executions.unwrap_or_else(|| Arc::new(DashMap::new())),
                domains: self.domains.unwrap_or_else(|| Arc::new(DashMap::new())),
                git_locks: self.git_locks.unwrap_or_else(|| Arc::new(DashMap::new())),
                session_event_locks: self
                    .session_event_locks
                    .unwrap_or_else(|| Arc::new(DashMap::new())),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use nenjo::{Manifest, ModelProviderFactory, Provider, provider::NoopToolFactory};

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

        assert!(harness.provider().manifest().agents.is_empty());
        harness
            .record_session_event(nenjo_sessions::SessionRuntimeEvent::Checkpoint(
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
        assert!(harness.provider().manifest().agents.is_empty());
    }

    fn assert_send_sync<T: Send + Sync>() {}
    fn assert_clone<T: Clone>() {}

    #[test]
    fn harness_is_send_sync_and_clone() {
        assert_send_sync::<super::Harness>();
        assert_clone::<super::Harness>();
    }
}
