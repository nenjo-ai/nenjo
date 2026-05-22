//! Platform-neutral manifest services for the harness.

use std::sync::Arc;

use nenjo::Manifest;
use tracing::{info, warn};

use crate::{Harness, ProviderRuntime, Result};

/// Facade for inspecting and replacing the provider manifest.
pub struct HarnessManifests<P: ProviderRuntime, SessionRt: nenjo_sessions::SessionRuntime> {
    harness: Harness<P, SessionRt>,
}

impl<P, SessionRt> HarnessManifests<P, SessionRt>
where
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
{
    pub(crate) fn new(harness: Harness<P, SessionRt>) -> Self {
        Self { harness }
    }

    /// Return the current manifest snapshot.
    pub fn snapshot(&self) -> Arc<Manifest> {
        self.harness.provider().manifest_snapshot()
    }

    /// Replace the running provider manifest and refresh active domain sessions.
    pub async fn replace(&self, manifest: Manifest) -> Result<()> {
        self.harness
            .swap_provider(self.harness.provider().with_manifest(manifest));
        self.refresh_active_domain_sessions().await;
        Ok(())
    }

    /// Rebuild active domain sessions against the current manifest.
    pub async fn refresh_active_domain_sessions(&self) {
        let domains = self.harness.domains();
        let active_sessions: Vec<_> = domains
            .iter()
            .map(|entry| {
                (
                    *entry.key(),
                    entry.agent_id,
                    entry.project_id,
                    entry.domain_command.clone(),
                )
            })
            .collect();

        for (session_id, agent_id, project_id, domain_command) in active_sessions {
            match self
                .harness
                .rebuild_domain_session(session_id, agent_id, project_id, &domain_command)
                .await
            {
                Ok(session) => {
                    domains.insert(session_id, session);
                    info!(%session_id, %agent_id, domain = %domain_command, "Refreshed active domain session after manifest update");
                }
                Err(error) => {
                    warn!(
                        %session_id,
                        %agent_id,
                        domain = %domain_command,
                        error = %error,
                        "Failed to refresh active domain session after manifest update"
                    );
                }
            }
        }
    }
}

impl<P, SessionRt> Clone for HarnessManifests<P, SessionRt>
where
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime,
{
    fn clone(&self) -> Self {
        Self {
            harness: self.harness.clone(),
        }
    }
}
