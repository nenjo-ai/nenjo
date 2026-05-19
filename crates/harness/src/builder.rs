//! Harness builder.

use std::sync::Arc;

use arc_swap::ArcSwap;
use dashmap::DashMap;

use crate::domain::DomainRegistry;
use crate::registry::ExecutionRegistry;
use crate::session::SessionEventLocks;
use crate::state::HarnessInner;
use crate::{Harness, ProviderRuntime};

/// Builder for the cloneable [`Harness`] handle.
pub struct HarnessBuilder<
    P: ProviderRuntime = nenjo::provider::ErasedProvider,
    SessionRt: nenjo_sessions::SessionRuntime = nenjo_sessions::NoopSessionRuntime,
> {
    provider: P,
    session_runtime: Arc<SessionRt>,
    executions: Option<ExecutionRegistry>,
    domains: Option<DomainRegistry<P>>,
    session_event_locks: Option<SessionEventLocks>,
}

impl<P> HarnessBuilder<P>
where
    P: ProviderRuntime,
{
    /// Create a builder around an assembled provider.
    pub fn new(provider: P) -> Self {
        Self {
            provider,
            session_runtime: Arc::new(nenjo_sessions::NoopSessionRuntime),
            executions: None,
            domains: None,
            session_event_locks: None,
        }
    }
}

impl<P, SessionRt> HarnessBuilder<P, SessionRt>
where
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime,
{
    /// Use a concrete session runtime for upserts, evidence events, and checkpoints.
    pub fn with_session_runtime<NextSessionRt>(
        self,
        session_runtime: NextSessionRt,
    ) -> HarnessBuilder<P, NextSessionRt>
    where
        NextSessionRt: nenjo_sessions::SessionRuntime,
    {
        HarnessBuilder {
            provider: self.provider,
            session_runtime: Arc::new(session_runtime),
            executions: self.executions,
            domains: self.domains,
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

    /// Use an existing session event lock registry. Hosts normally omit this.
    pub fn with_session_event_locks(mut self, session_event_locks: SessionEventLocks) -> Self {
        self.session_event_locks = Some(session_event_locks);
        self
    }

    /// Build the cloneable harness.
    pub fn build(self) -> Harness<P, SessionRt> {
        Harness {
            inner: Arc::new(HarnessInner {
                provider: Arc::new(ArcSwap::from_pointee(self.provider)),
                session_runtime: self.session_runtime,
                executions: self.executions.unwrap_or_else(|| Arc::new(DashMap::new())),
                domains: self.domains.unwrap_or_else(|| Arc::new(DashMap::new())),
                session_event_locks: self
                    .session_event_locks
                    .unwrap_or_else(|| Arc::new(DashMap::new())),
            }),
        }
    }
}
