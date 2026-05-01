use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use nenjo::client::NenjoClient;
use uuid::Uuid;

use crate::crypto::{AccountContentKey, WorkerAuthProvider};

/// Narrow interface that exposes only the `ACK` access needed by the envelope codec.
#[async_trait]
pub trait EnvelopeKeyProvider: Send + Sync + 'static {
    /// Returns the currently cached or persisted `ACK`, if one is available.
    async fn load_ack(&self) -> Result<Option<AccountContentKey>>;
    /// Returns the active `ACK`, refreshing enrollment state if needed.
    async fn load_or_refresh_ack(&self) -> Result<AccountContentKey>;
    /// Forces a refresh of enrollment-backed key state and returns the new `ACK`, if any.
    async fn refresh_ack(&self) -> Result<Option<AccountContentKey>>;
    /// Clears any in-memory key cache held by the provider.
    async fn clear_cached_ack(&self);
    /// Returns the current key version associated with the active wrapped `ACK`.
    async fn current_key_version(&self) -> Option<u32>;
}

/// Default worker-side [`EnvelopeKeyProvider`] backed by enrollment state.
pub struct EnrollmentBackedKeyProvider {
    auth_provider: Arc<WorkerAuthProvider>,
    api: Arc<NenjoClient>,
    api_key_id: Uuid,
}

impl EnrollmentBackedKeyProvider {
    /// Builds a key provider that reads local worker enrollment state and uses the
    /// authenticated API client to refresh it when required.
    pub fn new(
        auth_provider: impl Into<Arc<WorkerAuthProvider>>,
        api: Arc<NenjoClient>,
        api_key_id: Uuid,
    ) -> Self {
        Self {
            auth_provider: auth_provider.into(),
            api,
            api_key_id,
        }
    }
}

#[async_trait]
impl EnvelopeKeyProvider for EnrollmentBackedKeyProvider {
    async fn load_ack(&self) -> Result<Option<AccountContentKey>> {
        self.auth_provider.load_ack().await
    }

    async fn load_or_refresh_ack(&self) -> Result<AccountContentKey> {
        if let Some(ack) = self.auth_provider.load_ack().await? {
            return Ok(ack);
        }

        self.auth_provider
            .sync_worker_enrollment(self.api.as_ref(), self.api_key_id)
            .await?;

        self.auth_provider
            .load_ack()
            .await?
            .context("Encrypted chat content received before worker enrollment completed")
    }

    async fn refresh_ack(&self) -> Result<Option<AccountContentKey>> {
        self.auth_provider.clear_cached_ack().await;
        self.auth_provider
            .sync_worker_enrollment(self.api.as_ref(), self.api_key_id)
            .await?;
        self.auth_provider.load_ack().await
    }

    async fn clear_cached_ack(&self) {
        self.auth_provider.clear_cached_ack().await;
    }

    async fn current_key_version(&self) -> Option<u32> {
        self.auth_provider.current_key_version().await
    }
}
