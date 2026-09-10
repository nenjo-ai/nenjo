//! Bounded resource admission and execution identity shared by nested agent work.

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::sync::{Arc, Weak};
use std::time::Duration;

use parking_lot::Mutex;
use tokio::sync::Notify;
use uuid::Uuid;

/// A resource queue could not admit a request. These are local scheduling failures.
#[derive(Debug, thiserror::Error)]
pub enum AdmissionError {
    #[error("{pool}: queue is full (maximum {limit} waiting requests)")]
    QueueFull { pool: String, limit: usize },
    #[error("{pool}: queue deadline exceeded after {seconds} seconds")]
    QueueTimeout { pool: String, seconds: u64 },
}

/// A bounded pool that grants one slot per root in round-robin order.
#[derive(Clone, Debug)]
pub struct AdmissionPool(Arc<PoolInner>);

#[derive(Debug)]
struct PoolInner {
    name: String,
    limit: usize,
    max_queued: usize,
    timeout: Duration,
    state: Mutex<PoolState>,
}

#[derive(Debug, Default)]
struct PoolState {
    active: usize,
    next_ticket: u64,
    roots: VecDeque<Uuid>,
    queues: HashMap<Uuid, VecDeque<u64>>,
    waiters: HashMap<u64, Waiter>,
}

#[derive(Debug)]
struct Waiter {
    root: Uuid,
    granted: bool,
    notify: Arc<Notify>,
}

/// Releases capacity on completion, cancellation, or task abort.
#[derive(Debug)]
pub struct AdmissionPermit {
    pool: AdmissionPool,
    ticket: u64,
}

impl AdmissionPool {
    /// Construct a pool. A zero queue length permits immediate admission only.
    /// Panics if the active limit or queue deadline is zero.
    pub fn new(
        name: impl Into<String>,
        limit: usize,
        max_queued: usize,
        timeout: Duration,
    ) -> Self {
        assert!(limit > 0, "admission concurrency must be positive");
        assert!(
            !timeout.is_zero(),
            "admission queue deadline must be positive"
        );
        Self(Arc::new(PoolInner {
            name: name.into(),
            limit,
            max_queued,
            timeout,
            state: Mutex::new(PoolState::default()),
        }))
    }

    /// Maximum number of simultaneous resource holders.
    pub fn limit(&self) -> usize {
        self.0.limit
    }

    /// Wait for capacity, fairly sharing pending requests among root executions.
    pub async fn acquire(&self) -> Result<AdmissionPermit, AdmissionError> {
        self.acquire_for(current_root_id().unwrap_or_else(Uuid::new_v4))
            .await
    }

    /// Admit a request attributed to an explicit root execution.
    pub async fn acquire_for(&self, root: Uuid) -> Result<AdmissionPermit, AdmissionError> {
        let notify = Arc::new(Notify::new());
        let ticket = {
            let mut state = self.0.state.lock();
            if state.active >= self.0.limit
                && state.waiters.len() - state.active >= self.0.max_queued
            {
                return Err(AdmissionError::QueueFull {
                    pool: self.0.name.clone(),
                    limit: self.0.max_queued,
                });
            }
            let ticket = state.next_ticket;
            state.next_ticket += 1;
            state.waiters.insert(
                ticket,
                Waiter {
                    root,
                    granted: false,
                    notify: notify.clone(),
                },
            );
            if !state.queues.contains_key(&root) {
                state.roots.push_back(root);
            }
            state.queues.entry(root).or_default().push_back(ticket);
            self.dispatch(&mut state);
            ticket
        };
        // The same guard cleans up a queued ticket and a granted-but-unconsumed slot.
        let permit = AdmissionPermit {
            pool: self.clone(),
            ticket,
        };
        let queued = !self.0.state.lock().waiters[&ticket].granted;
        let queued_at = std::time::Instant::now();
        if queued {
            emit_capacity(crate::TurnEvent::ResourceCapacityWaiting {
                pool: self.0.name.clone(),
                limit: self.0.limit,
            });
            tracing::debug!(pool = self.0.name, %root, limit = self.0.limit, "Execution waiting for capacity");
        }
        let wait = async {
            loop {
                let notified = notify.notified();
                if self.0.state.lock().waiters[&ticket].granted {
                    return;
                }
                notified.await;
            }
        };
        tokio::time::timeout(self.0.timeout, wait)
            .await
            .map_err(|_| AdmissionError::QueueTimeout {
                pool: self.0.name.clone(),
                seconds: self.0.timeout.as_secs(),
            })?;
        if queued {
            emit_capacity(crate::TurnEvent::ResourceCapacityAcquired {
                pool: self.0.name.clone(),
            });
            tracing::debug!(pool = self.0.name, %root, queued_ms = queued_at.elapsed().as_millis(), "Execution acquired capacity");
        }
        Ok(permit)
    }

    fn dispatch(&self, state: &mut PoolState) {
        while state.active < self.0.limit {
            let Some(root) = state.roots.pop_front() else {
                break;
            };
            let queue = state.queues.get_mut(&root).expect("queued root exists");
            let ticket = queue.pop_front().expect("root queue is nonempty");
            if queue.is_empty() {
                state.queues.remove(&root);
            } else {
                state.roots.push_back(root);
            }
            let waiter = state
                .waiters
                .get_mut(&ticket)
                .expect("queued ticket exists");
            waiter.granted = true;
            state.active += 1;
            waiter.notify.notify_one();
        }
    }
}

impl Drop for AdmissionPermit {
    fn drop(&mut self) {
        let mut state = self.pool.0.state.lock();
        if let Some(waiter) = state.waiters.remove(&self.ticket) {
            if waiter.granted {
                state.active -= 1;
            } else if let Some(queue) = state.queues.get_mut(&waiter.root) {
                queue.retain(|ticket| *ticket != self.ticket);
                if queue.is_empty() {
                    state.queues.remove(&waiter.root);
                    state.roots.retain(|root| *root != waiter.root);
                }
            }
        }
        self.pool.dispatch(&mut state);
    }
}

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

    /// Poll a future with this execution identity.
    pub async fn scope<F: Future>(&self, future: F) -> F::Output {
        EXECUTION.scope(self.clone(), future).await
    }
}

fn emit_capacity(event: crate::TurnEvent) {
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

/// Bound a runnable phase. No permit is retained between phases or while waiting for children.
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

/// Preserve an execution scope through a task spawn, optionally making it a descendant.
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

    fn pool(queued: usize) -> AdmissionPool {
        AdmissionPool::new("test", 1, queued, Duration::from_millis(100))
    }

    #[tokio::test]
    async fn queue_bound_and_cancelled_grant_do_not_leak_capacity() {
        let pool = pool(1);
        let held = pool.acquire().await.unwrap();
        let mut queued = Box::pin(pool.acquire());
        assert!(futures_util::poll!(queued.as_mut()).is_pending());
        assert!(matches!(
            pool.acquire().await,
            Err(AdmissionError::QueueFull { .. })
        ));
        drop(held); // The queued request is granted, but has not been polled again.
        drop(queued);
        let recovered = pool.acquire().await.unwrap();
        drop(recovered);
        assert!(pool.0.state.lock().waiters.is_empty());
    }

    #[tokio::test]
    async fn queue_deadline_removes_waiter() {
        let pool = pool(1);
        let held = pool.acquire().await.unwrap();
        assert!(matches!(
            pool.acquire().await,
            Err(AdmissionError::QueueTimeout { .. })
        ));
        assert_eq!(pool.0.state.lock().waiters.len(), 1);
        drop(held);
        pool.acquire().await.unwrap();
    }

    #[tokio::test]
    async fn pending_roots_receive_round_robin_slots() {
        let pool = pool(8);
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let held = pool.acquire_for(a).await.unwrap();
        let mut a1 = Box::pin(pool.acquire_for(a));
        let mut a2 = Box::pin(pool.acquire_for(a));
        let mut b1 = Box::pin(pool.acquire_for(b));
        assert!(futures_util::poll!(a1.as_mut()).is_pending());
        assert!(futures_util::poll!(a2.as_mut()).is_pending());
        assert!(futures_util::poll!(b1.as_mut()).is_pending());
        drop(held);
        let first = a1.await.unwrap();
        drop(first);
        assert!(futures_util::poll!(a2.as_mut()).is_pending());
        let second = b1.await.unwrap();
        drop(second);
        a2.await.unwrap();
    }

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
        assert!(root.tree.0.state.lock().waiters.is_empty());
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
}
