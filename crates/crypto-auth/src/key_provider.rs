use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use nenjo::client::NenjoClient;
use uuid::Uuid;

use crate::{ContentKey, ContentScope, WorkerAuthProvider};

#[async_trait]
pub trait EnvelopeKeyProvider: Send + Sync + 'static {
    /// Load the currently available content key for the requested scope.
    async fn load_key(&self, scope: ContentScope) -> Result<Option<ContentKey>>;
    /// Load the scope key, syncing enrollment from the backend if necessary.
    async fn load_or_refresh_key(&self, scope: ContentScope) -> Result<ContentKey>;
    /// Force a backend refresh for the requested scope key.
    async fn refresh_key(&self, scope: ContentScope) -> Result<Option<ContentKey>>;
    /// Clear any local in-memory cache for the requested scope key.
    async fn clear_cached_key(&self, scope: ContentScope);
    /// Return the key version currently associated with the requested scope.
    async fn current_key_version(&self, scope: ContentScope) -> Option<u32>;
    /// Load a user-scoped key for a specific actor.
    async fn load_user_key(&self, user_id: Uuid) -> Result<Option<ContentKey>>;
    /// Load a user-scoped key, syncing enrollment from the backend if necessary.
    async fn load_or_refresh_user_key(&self, user_id: Uuid) -> Result<ContentKey>;
    /// Force a backend refresh for a user-scoped key.
    async fn refresh_user_key(&self, user_id: Uuid) -> Result<Option<ContentKey>>;
    /// Return the key version currently associated with the actor-scoped key.
    async fn current_user_key_version(&self, user_id: Uuid) -> Option<u32>;
}

/// Default worker-side [`EnvelopeKeyProvider`] backed by enrollment state.
pub struct EnrollmentBackedKeyProvider {
    auth_provider: Arc<WorkerAuthProvider>,
    api: Arc<NenjoClient>,
    api_key_id: Uuid,
    bootstrap_user_id: Uuid,
}

impl EnrollmentBackedKeyProvider {
    /// Construct a key provider using local worker enrollment state plus a
    /// platform API client for on-demand refresh.
    pub fn new(
        auth_provider: impl Into<Arc<WorkerAuthProvider>>,
        api: impl Into<Arc<NenjoClient>>,
        api_key_id: Uuid,
        bootstrap_user_id: Uuid,
    ) -> Self {
        Self {
            auth_provider: auth_provider.into(),
            api: api.into(),
            api_key_id,
            bootstrap_user_id,
        }
    }
}

#[async_trait]
impl EnvelopeKeyProvider for EnrollmentBackedKeyProvider {
    async fn load_key(&self, scope: ContentScope) -> Result<Option<ContentKey>> {
        match scope {
            ContentScope::User => Ok(None),
            ContentScope::Org => self.auth_provider.load_ock().await,
        }
    }

    async fn load_or_refresh_key(&self, scope: ContentScope) -> Result<ContentKey> {
        if let Some(key) = self.load_key(scope).await? {
            return Ok(key);
        }

        self.auth_provider
            .sync_worker_enrollment(
                self.api.as_ref(),
                self.api_key_id,
                self.bootstrap_user_id,
                None,
            )
            .await?;

        self.load_key(scope).await?.context(match scope {
            ContentScope::User => {
                "Encrypted chat content received before worker enrollment completed"
            }
            ContentScope::Org => {
                "Encrypted org content received before worker OCK enrollment completed"
            }
        })
    }

    async fn refresh_key(&self, scope: ContentScope) -> Result<Option<ContentKey>> {
        self.clear_cached_key(scope).await;
        self.auth_provider
            .sync_worker_enrollment(
                self.api.as_ref(),
                self.api_key_id,
                self.bootstrap_user_id,
                None,
            )
            .await?;
        self.load_key(scope).await
    }

    async fn clear_cached_key(&self, scope: ContentScope) {
        match scope {
            ContentScope::User => {}
            ContentScope::Org => self.auth_provider.clear_cached_ock().await,
        }
    }

    async fn current_key_version(&self, scope: ContentScope) -> Option<u32> {
        match scope {
            ContentScope::User => None,
            ContentScope::Org => self.auth_provider.current_ock_key_version().await,
        }
    }

    async fn load_user_key(&self, user_id: Uuid) -> Result<Option<ContentKey>> {
        self.auth_provider.load_ack_for_user(user_id).await
    }

    async fn load_or_refresh_user_key(&self, user_id: Uuid) -> Result<ContentKey> {
        if let Some(key) = self.load_user_key(user_id).await? {
            return Ok(key);
        }

        self.auth_provider
            .sync_worker_enrollment(
                self.api.as_ref(),
                self.api_key_id,
                self.bootstrap_user_id,
                None,
            )
            .await?;

        self.load_user_key(user_id)
            .await?
            .context("Encrypted chat content received before sender ACK sync completed")
    }

    async fn refresh_user_key(&self, user_id: Uuid) -> Result<Option<ContentKey>> {
        self.auth_provider.clear_cached_ack_for_user(user_id).await;
        self.auth_provider
            .sync_worker_enrollment(
                self.api.as_ref(),
                self.api_key_id,
                self.bootstrap_user_id,
                None,
            )
            .await?;
        self.load_user_key(user_id).await
    }

    async fn current_user_key_version(&self, user_id: Uuid) -> Option<u32> {
        self.auth_provider
            .current_key_version_for_user(user_id)
            .await
    }
}
