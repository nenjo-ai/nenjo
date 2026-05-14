//! Worker crypto/account-key handlers.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use nenjo_events::WrappedAccountContentKey;
use uuid::Uuid;

use crate::execution_trace::ExecutionTraceRuntime;
use crate::{Harness, HarnessProvider};

#[async_trait]
pub trait AccountKeyStore: Send + Sync {
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

impl<P, SessionRt, TraceRt, StoreRt, McpRt> Harness<P, SessionRt, TraceRt, StoreRt, McpRt>
where
    P: HarnessProvider,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
    TraceRt: ExecutionTraceRuntime + 'static,
    StoreRt: crate::handlers::manifest::ManifestStore + 'static,
    McpRt: crate::handlers::manifest::McpRuntime + 'static,
{
    pub async fn handle_worker_account_key_updated<K>(
        &self,
        ctx: &CryptoCommandContext<K>,
        wrapped_ack: WrappedAccountContentKey,
    ) -> Result<()>
    where
        K: AccountKeyStore,
    {
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
