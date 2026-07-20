//! Parent-session-scoped conversation history for abilities.

use std::sync::Arc;

use dashmap::DashMap;
use nenjo_models::ChatMessage;
use parking_lot::RwLock;
use tokio::sync::{Mutex, OwnedMutexGuard};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct AbilitySessionKey {
    parent_session_id: Uuid,
    ability_name: String,
}

#[derive(Debug)]
struct AbilitySessionLane {
    invocation: Arc<Mutex<()>>,
    history: RwLock<Vec<ChatMessage>>,
}

impl AbilitySessionLane {
    fn new(history: Vec<ChatMessage>) -> Self {
        Self {
            invocation: Arc::new(Mutex::new(())),
            history: RwLock::new(history),
        }
    }
}

/// Shared ability histories, isolated by parent session and ability name.
///
/// An agent execution owns this live store. The harness hydrates it from the
/// durable parent transcript whenever it rebuilds the runner for a new turn.
#[derive(Debug, Default)]
pub struct AbilitySessionStore {
    lanes: DashMap<AbilitySessionKey, Arc<AbilitySessionLane>>,
}

impl AbilitySessionStore {
    /// Seed histories reconstructed from a persisted parent transcript.
    ///
    /// Existing lanes win because they may contain a more recent invocation
    /// whose transcript events are still queued for persistence.
    pub fn hydrate<I>(&self, parent_session_id: Uuid, histories: I)
    where
        I: IntoIterator<Item = (String, Vec<ChatMessage>)>,
    {
        for (ability_name, history) in histories {
            if history.is_empty() {
                continue;
            }
            self.lanes
                .entry(AbilitySessionKey {
                    parent_session_id,
                    ability_name,
                })
                .or_insert_with(|| Arc::new(AbilitySessionLane::new(history)));
        }
    }

    pub(crate) async fn begin(
        &self,
        parent_session_id: Uuid,
        ability_name: &str,
    ) -> AbilitySessionInvocation {
        let lane = self
            .lanes
            .entry(AbilitySessionKey {
                parent_session_id,
                ability_name: ability_name.to_string(),
            })
            .or_insert_with(|| Arc::new(AbilitySessionLane::new(Vec::new())))
            .clone();
        let guard = lane.invocation.clone().lock_owned().await;
        let history = lane.history.read().clone();
        AbilitySessionInvocation {
            lane,
            _guard: guard,
            history,
        }
    }
}

/// Exclusive invocation lease for one ability in one parent session.
///
/// Serializing a lane prevents concurrent invocations from overwriting each
/// other's conversation history while leaving unrelated abilities concurrent.
pub(crate) struct AbilitySessionInvocation {
    lane: Arc<AbilitySessionLane>,
    _guard: OwnedMutexGuard<()>,
    history: Vec<ChatMessage>,
}

impl AbilitySessionInvocation {
    pub(crate) fn history(&self) -> &[ChatMessage] {
        &self.history
    }

    pub(crate) fn append_exchange(&self, input: String, output: String) {
        let mut history = self.lane.history.write();
        history.push(ChatMessage::user(input));
        history.push(ChatMessage::assistant(output));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn histories_are_isolated_by_parent_session_and_ability() {
        let store = AbilitySessionStore::default();
        let first_parent = Uuid::new_v4();
        let second_parent = Uuid::new_v4();

        let first = store.begin(first_parent, "research").await;
        first.append_exchange("first question".into(), "first answer".into());
        drop(first);

        assert_eq!(
            store.begin(first_parent, "research").await.history().len(),
            2
        );
        assert!(
            store
                .begin(first_parent, "writer")
                .await
                .history()
                .is_empty()
        );
        assert!(
            store
                .begin(second_parent, "research")
                .await
                .history()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn hydration_does_not_replace_live_history() {
        let store = AbilitySessionStore::default();
        let parent = Uuid::new_v4();
        let live = store.begin(parent, "research").await;
        live.append_exchange("live".into(), "answer".into());
        drop(live);

        store.hydrate(
            parent,
            [("research".into(), vec![ChatMessage::user("stale")])],
        );

        let invocation = store.begin(parent, "research").await;
        assert_eq!(invocation.history()[0].content, "live");
    }
}
