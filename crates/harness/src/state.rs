//! Internal harness state shared by cloned handles.

use std::sync::Arc;

use arc_swap::ArcSwap;
use nenjo_sessions::SessionRuntime;

use crate::ProviderRuntime;
use crate::domain::DomainRegistry;
use crate::registry::ExecutionRegistry;
use crate::session::SessionEventLocks;

pub(crate) struct HarnessInner<
    P: ProviderRuntime = nenjo::provider::ErasedProvider,
    SessionRt: SessionRuntime = nenjo_sessions::NoopSessionRuntime,
> {
    pub(crate) provider: Arc<ArcSwap<P>>,
    pub(crate) session_runtime: Arc<SessionRt>,
    pub(crate) executions: ExecutionRegistry,
    pub(crate) domains: DomainRegistry<P>,
    pub(crate) session_event_locks: SessionEventLocks,
}
