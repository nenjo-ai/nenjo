//! Execution-tree identity and phase-scoped descendant admission.

use std::future::Future;
use std::sync::{Arc, Weak};
use std::time::Duration;

use uuid::Uuid;

use super::admission::{AdmissionError, AdmissionPermit, AdmissionPool};

tokio::task_local! { static EXECUTION: ExecutionContext; }

/// Execution-tree identity and runnable descendant budgets, inherited explicitly across spawns.
#[derive(Clone, Debug)]
pub struct ExecutionContext {
    root: Uuid,
    tree: AdmissionPool,
    children: AdmissionPool,
    parent: Option<AdmissionPool>,
    per_parent: usize,
    max_queued: usize,
    timeout: Duration,
    activity: Arc<tokio::sync::Mutex<Weak<RunnableLease>>>,
    events: Option<tokio::sync::mpsc::UnboundedSender<crate::TurnEvent>>,
}

#[derive(Debug)]
struct RunnableLease {
    _parent: AdmissionPermit,
    _tree: AdmissionPermit,
}

impl ExecutionContext {
    /// Start a root with a shared descendant budget and immediate-child budget.
    ///
    /// The root itself bypasses these budgets; root admission is configured separately.
    ///
    /// # Panics
    /// Panics if either active budget or the queue deadline is zero.
    pub fn root(
        descendants: usize,
        per_parent: usize,
        max_queued: usize,
        timeout: Duration,
    ) -> Self {
        Self {
            root: Uuid::new_v4(),
            tree: AdmissionPool::new("execution tree", descendants, max_queued, timeout),
            children: AdmissionPool::new("parent children", per_parent, max_queued, timeout),
            parent: None,
            per_parent,
            max_queued,
            timeout,
            activity: Arc::new(tokio::sync::Mutex::new(Weak::new())),
            events: None,
        }
    }

    /// Retain the first event sink so descendants report capacity to the root run.
    pub(crate) fn with_events(
        mut self,
        events: tokio::sync::mpsc::UnboundedSender<crate::TurnEvent>,
    ) -> Self {
        self.events.get_or_insert(events);
        self
    }

    /// Capture the current scope before spawning another Tokio task.
    pub fn current() -> Option<Self> {
        EXECUTION.try_with(Clone::clone).ok()
    }

    /// Share the root budget while assigning this parent's immediate-child budget.
    pub fn child(&self) -> Self {
        Self {
            children: AdmissionPool::new(
                "parent children",
                self.per_parent,
                self.max_queued,
                self.timeout,
            ),
            parent: Some(self.children.clone()),
            activity: Arc::new(tokio::sync::Mutex::new(Weak::new())),
            ..self.clone()
        }
    }

    /// Poll a future with this execution identity, restoring the caller scope afterward.
    ///
    /// Tokio task locals are not inherited by spawned tasks. Capture [`Self::current`]
    /// and explicitly scope the spawned future to preserve its root attribution.
    pub async fn scope<F: Future>(&self, future: F) -> F::Output {
        EXECUTION.scope(self.clone(), future).await
    }
}

/// Send admission diagnostics to the captured root sink when running in a scope.
pub(super) fn emit_capacity(event: crate::TurnEvent) {
    let _ = EXECUTION.try_with(|context| {
        if let Some(events) = &context.events {
            let _ = events.send(event);
        }
    });
}

/// Root identity used by worker resource queues; absent outside an agent execution.
pub fn current_root_id() -> Option<Uuid> {
    EXECUTION.try_with(|context| context.root).ok()
}

/// Bound a runnable phase, sharing one lease among concurrent work in this execution.
///
/// Roots do not consume descendant capacity. A child acquires its immediate-parent
/// permit before its tree permit and releases both when its last active phase
/// finishes or is cancelled. Harness controls that wait for descendants must run
/// outside this helper, so the child being awaited can acquire the tree slot.
pub(crate) async fn runnable<F: Future>(future: F) -> Result<F::Output, AdmissionError> {
    let context = ExecutionContext::current();
    let _lease = if let Some(context) = context
        && let Some(parent) = &context.parent
    {
        // Parallel tools in the same execution share one runnable slot.
        let mut activity = context.activity.lock().await;
        let lease = if let Some(lease) = activity.upgrade() {
            lease
        } else {
            let lease = Arc::new(RunnableLease {
                _parent: parent.acquire_for(context.root).await?,
                _tree: context.tree.acquire_for(context.root).await?,
            });
            *activity = Arc::downgrade(&lease);
            lease
        };
        Some(lease)
    } else {
        None
    };
    Ok(future.await)
}

/// Run in a captured scope, or directly when the caller has no execution context.
pub(crate) async fn in_scope<F: Future>(context: Option<ExecutionContext>, future: F) -> F::Output {
    if let Some(context) = context {
        context.scope(future).await
    } else {
        future.await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn waiting_parent_yields_tree_capacity_and_parallel_tools_share_one_slot() {
        let root = ExecutionContext::root(1, 1, 8, Duration::from_secs(1));
        let parent = root.child();
        let descendant = parent.child();
        root.scope(async {
            let root_id = current_root_id();
            parent
                .scope(async {
                    assert_eq!(current_root_id(), root_id);
                    let (one, two) = tokio::join!(
                        runnable(async {
                            tokio::task::yield_now().await;
                            1
                        }),
                        runnable(async {
                            tokio::task::yield_now().await;
                            2
                        }),
                    );
                    assert_eq!((one.unwrap(), two.unwrap()), (1, 2));
                    // Parent waits outside a runnable phase; a grandchild can use the only tree slot.
                    descendant
                        .scope(async { runnable(async { 3 }).await.unwrap() })
                        .await;
                    runnable(async { 4 }).await.unwrap();
                })
                .await;
        })
        .await;
        root.tree.acquire().await.unwrap();
    }

    #[tokio::test]
    async fn sibling_and_grandchild_share_tree_budget() {
        let root = ExecutionContext::root(1, 3, 8, Duration::from_secs(1));
        let first = root.child();
        let sibling = root.child();
        let grandchild = first.child();
        let held = root.tree.acquire().await.unwrap();
        let mut sibling_work = Box::pin(sibling.scope(runnable(async { 1 })));
        let mut grandchild_work = Box::pin(grandchild.scope(runnable(async { 2 })));
        assert!(futures_util::poll!(sibling_work.as_mut()).is_pending());
        assert!(futures_util::poll!(grandchild_work.as_mut()).is_pending());
        drop(held);
        assert_eq!(sibling_work.await.unwrap(), 1);
        assert_eq!(grandchild_work.await.unwrap(), 2);
    }

    #[tokio::test]
    async fn cancellation_during_tree_admission_releases_the_parent_permit() {
        let root = ExecutionContext::root(1, 1, 1, Duration::from_secs(1));
        let first = root.child();
        let sibling = root.child();
        let held = root.tree.acquire().await.unwrap();
        let mut first_work = Box::pin(first.scope(runnable(async { 1 })));
        let mut sibling_work = Box::pin(sibling.scope(runnable(async { 2 })));
        assert!(futures_util::poll!(first_work.as_mut()).is_pending());
        assert!(futures_util::poll!(sibling_work.as_mut()).is_pending());
        drop(first_work);
        assert!(futures_util::poll!(sibling_work.as_mut()).is_pending());
        drop(held);
        assert_eq!(sibling_work.await.unwrap(), 2);
        root.children.acquire().await.unwrap();
        root.tree.acquire().await.unwrap();
    }

    #[tokio::test]
    async fn parallel_phases_keep_the_shared_lease_until_the_last_phase_finishes() {
        let root = ExecutionContext::root(1, 1, 1, Duration::from_secs(1));
        let child = root.child();
        let sibling = root.child();
        let mut long_phase = Box::pin(child.scope(runnable(std::future::pending::<()>())));
        assert!(futures_util::poll!(long_phase.as_mut()).is_pending());
        assert_eq!(child.scope(runnable(async { 2 })).await.unwrap(), 2);
        let mut sibling_work = Box::pin(sibling.scope(runnable(async { 3 })));
        assert!(futures_util::poll!(sibling_work.as_mut()).is_pending());
        drop(long_phase);
        assert_eq!(sibling_work.await.unwrap(), 3);
    }

    #[tokio::test]
    async fn spawned_descendants_preserve_root_identity_and_restore_the_caller_scope() {
        assert!(current_root_id().is_none());
        let root = ExecutionContext::root(1, 1, 1, Duration::from_secs(1));
        let other = ExecutionContext::root(1, 1, 1, Duration::from_secs(1));
        root.scope(async {
            assert_eq!(current_root_id(), Some(root.root));
            let spawned = tokio::spawn(in_scope(Some(root.child()), async { current_root_id() }));
            assert_eq!(spawned.await.unwrap(), Some(root.root));
            other
                .scope(async { assert_eq!(current_root_id(), Some(other.root)) })
                .await;
            assert_eq!(current_root_id(), Some(root.root));
        })
        .await;
        assert!(current_root_id().is_none());
    }

    #[tokio::test]
    async fn queued_admission_reports_waiting_and_acquired_once() {
        let (events, mut received) = tokio::sync::mpsc::unbounded_channel();
        let root = ExecutionContext::root(1, 1, 1, Duration::from_secs(1)).with_events(events);
        let pool = AdmissionPool::new("model", 1, 1, Duration::from_secs(1));
        root.scope(async {
            let held = pool.acquire().await.unwrap();
            assert!(received.try_recv().is_err());
            let mut queued = Box::pin(pool.acquire());
            assert!(futures_util::poll!(queued.as_mut()).is_pending());
            assert!(matches!(received.try_recv().unwrap(),
                crate::TurnEvent::ResourceCapacityWaiting { pool, limit: 1 } if pool == "model"));
            drop(held);
            queued.await.unwrap();
            assert!(matches!(received.try_recv().unwrap(),
                crate::TurnEvent::ResourceCapacityAcquired { pool } if pool == "model"));
            assert!(received.try_recv().is_err());
        })
        .await;
    }
}
