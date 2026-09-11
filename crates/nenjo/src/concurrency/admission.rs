//! Fair, bounded admission with cancellation-safe ownership of queue tickets.
//!
//! `waiters` owns every ticket until its permit is dropped. `queues` and `roots`
//! index only waiting tickets. The state lock protects both indexes and the
//! active count, including grants whose acquisition future has not resumed yet.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use tokio::sync::Notify;
use uuid::Uuid;

use super::execution::{current_root_id, emit_capacity};

/// A resource queue could not admit a request. These are local scheduling failures.
#[derive(Debug, thiserror::Error)]
pub enum AdmissionError {
    #[error("{pool}: queue is full (maximum {limit} waiting requests)")]
    QueueFull { pool: String, limit: usize },
    #[error("{pool}: queue deadline exceeded after {seconds} seconds")]
    QueueTimeout { pool: String, seconds: u64 },
}

/// A bounded pool that rotates among roots when granting queued requests.
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
    status: WaiterStatus,
    notify: Arc<Notify>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WaiterStatus {
    Queued,
    Granted,
}

/// Release the owned queue ticket or capacity grant when dropped, including on task abort.
#[derive(Debug)]
pub struct AdmissionPermit {
    pool: AdmissionPool,
    ticket: u64,
}

impl AdmissionPool {
    /// Construct a pool. A zero queue length permits immediate admission only.
    ///
    /// # Panics
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
    ///
    /// Queue bounds count waiting tickets only. FIFO ordering is preserved within
    /// each root; roots take turns when a slot becomes available. Dropping this
    /// future removes its ticket even if it was granted but not yet consumed.
    ///
    /// Returns [`AdmissionError::QueueFull`] without waiting when the queue is full,
    /// or [`AdmissionError::QueueTimeout`] when the queued request exceeds its deadline.
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
                    status: WaiterStatus::Queued,
                    notify: notify.clone(),
                },
            );
            if !state.queues.contains_key(&root) {
                state.roots.push_back(root);
            }
            state.queues.entry(root).or_default().push_back(ticket);
            state.dispatch(self.0.limit);
            ticket
        };
        // The same guard cleans up a queued ticket and a granted-but-unconsumed slot.
        let permit = AdmissionPermit {
            pool: self.clone(),
            ticket,
        };
        let queued = self.0.state.lock().waiters[&ticket].status == WaiterStatus::Queued;
        let queued_at = Instant::now();
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
                if self.0.state.lock().waiters[&ticket].status == WaiterStatus::Granted {
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
}

impl PoolState {
    /// Remove either a queued ticket or a grant, maintaining both queue indexes.
    fn remove(&mut self, ticket: u64) {
        if let Some(waiter) = self.waiters.remove(&ticket) {
            if waiter.status == WaiterStatus::Granted {
                self.active -= 1;
            } else if let Some(queue) = self.queues.get_mut(&waiter.root) {
                queue.retain(|queued_ticket| *queued_ticket != ticket);
                if queue.is_empty() {
                    self.queues.remove(&waiter.root);
                    self.roots.retain(|root| *root != waiter.root);
                }
            }
        }
    }

    /// Grant FIFO tickets while rotating among roots that still have queued work.
    fn dispatch(&mut self, limit: usize) {
        while self.active < limit {
            let Some(root) = self.roots.pop_front() else {
                break;
            };
            let queue = self.queues.get_mut(&root).expect("queued root exists");
            let ticket = queue.pop_front().expect("root queue is nonempty");
            if queue.is_empty() {
                self.queues.remove(&root);
            } else {
                self.roots.push_back(root);
            }
            let waiter = self.waiters.get_mut(&ticket).expect("queued ticket exists");
            waiter.status = WaiterStatus::Granted;
            self.active += 1;
            waiter.notify.notify_one();
        }
    }
}

impl Drop for AdmissionPermit {
    fn drop(&mut self) {
        let mut state = self.pool.0.state.lock();
        state.remove(self.ticket);
        state.dispatch(self.pool.0.limit);
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

    #[tokio::test(start_paused = true)]
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
    async fn zero_queue_allows_immediate_capacity_only() {
        let pool = pool(0);
        let held = pool.acquire().await.unwrap();
        assert!(matches!(
            pool.acquire().await,
            Err(AdmissionError::QueueFull { limit: 0, .. })
        ));
        drop(held);
        pool.acquire().await.unwrap();
    }

    #[tokio::test]
    async fn cancelling_a_queued_root_preserves_fifo_for_remaining_roots() {
        let pool = pool(3);
        let held = pool.acquire().await.unwrap();
        let cancelled_root = Uuid::new_v4();
        let remaining_root = Uuid::new_v4();
        let mut cancelled = Box::pin(pool.acquire_for(cancelled_root));
        let mut first = Box::pin(pool.acquire_for(remaining_root));
        let mut second = Box::pin(pool.acquire_for(remaining_root));
        assert!(futures_util::poll!(cancelled.as_mut()).is_pending());
        assert!(futures_util::poll!(first.as_mut()).is_pending());
        assert!(futures_util::poll!(second.as_mut()).is_pending());
        drop(cancelled);
        drop(held);
        let first = first.await.unwrap();
        assert!(futures_util::poll!(second.as_mut()).is_pending());
        drop(first);
        drop(second.await.unwrap());
        let state = pool.0.state.lock();
        assert_eq!(state.active, 0);
        assert!(state.waiters.is_empty());
        assert!(state.queues.is_empty());
        assert!(state.roots.is_empty());
    }

    #[tokio::test]
    async fn aborting_a_task_releases_an_acquired_permit() {
        let pool = pool(0);
        let task_pool = pool.clone();
        let (started, ready) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let _permit = task_pool.acquire().await.unwrap();
            started.send(()).unwrap();
            std::future::pending::<()>().await;
        });
        ready.await.unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        pool.acquire().await.unwrap();
    }
}
