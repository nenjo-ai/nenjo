//! Worker crypto/account-key handlers.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use nenjo_events::WrappedAccountContentKey;
use uuid::Uuid;

use nenjo_harness::{Harness, ProviderRuntime};

#[async_trait]
/// Stores user-scoped wrapped account content keys for the worker.
///
/// Platform key update events are decoded by the worker and handed to this
/// trait so the concrete runtime can persist them in its local auth/key state.
pub trait AccountKeyStore: Send + Sync {
    /// Persist the wrapped account content key for `user_id`.
    async fn store_user_ack(
        &self,
        user_id: Uuid,
        wrapped_ack: WrappedAccountContentKey,
    ) -> Result<()>;
}

#[derive(Clone)]
pub struct CryptoCommandContext<K> {
    pub actor_user_id: Uuid,
    pub account_keys: K,
}

#[async_trait]
/// Worker integration methods for crypto/account-key platform events.
pub(crate) trait WorkerCryptoHarnessExt<K>
where
    K: AccountKeyStore,
{
    /// Handle a platform notification that the account content key changed.
    async fn handle_worker_account_key_updated(
        &self,
        ctx: &CryptoCommandContext<K>,
        wrapped_ack: WrappedAccountContentKey,
    ) -> Result<()>;
}

#[async_trait]
impl<P, SessionRt, K> WorkerCryptoHarnessExt<K> for Harness<P, SessionRt>
where
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
    K: AccountKeyStore,
{
    async fn handle_worker_account_key_updated(
        &self,
        ctx: &CryptoCommandContext<K>,
        wrapped_ack: WrappedAccountContentKey,
    ) -> Result<()> {
        ctx.account_keys
            .store_user_ack(ctx.actor_user_id, wrapped_ack)
            .await
    }
}

#[async_trait]
impl<T> AccountKeyStore for Arc<T>
where
    T: AccountKeyStore + ?Sized,
{
    async fn store_user_ack(
        &self,
        user_id: Uuid,
        wrapped_ack: WrappedAccountContentKey,
    ) -> Result<()> {
        self.as_ref().store_user_ack(user_id, wrapped_ack).await
    }
}
