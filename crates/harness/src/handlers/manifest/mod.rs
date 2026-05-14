//! Manifest change handler — incremental resource updates.
mod apply;
mod delete;
mod fetch;
mod inline;
mod payload;
mod services;

use tracing::{info, warn};
use uuid::Uuid;

use nenjo_events::{EncryptedPayload, ResourceAction, ResourceType};

use crate::execution_trace::ExecutionTraceRuntime;
use crate::{Harness, HarnessError, HarnessProvider, Result};
use apply::{ManifestChange, apply_manifest_change};
pub use services::{
    ManifestServices, ManifestStore, McpRuntime, NoopManifestStore, NoopMcpRuntime,
};

/// Handle a manifest.changed event.
///
/// Fetches only the changed resource and applies an incremental update to
/// the manifest. Falls back to a full refresh if the fetch fails.
impl<P, SessionRt, TraceRt, StoreRt, McpRt> Harness<P, SessionRt, TraceRt, StoreRt, McpRt>
where
    P: HarnessProvider,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
    TraceRt: ExecutionTraceRuntime + 'static,
    StoreRt: crate::handlers::manifest::ManifestStore + 'static,
    McpRt: crate::handlers::manifest::McpRuntime + 'static,
{
    pub async fn handle_manifest_changed(
        &self,
        resource_type: ResourceType,
        resource_id: Uuid,
        action: ResourceAction,
        project_id: Option<Uuid>,
        payload: Option<serde_json::Value>,
        encrypted_payload: Option<EncryptedPayload>,
    ) -> Result<()> {
        handle_manifest_changed(
            self,
            resource_type,
            resource_id,
            action,
            project_id,
            payload,
            encrypted_payload,
        )
        .await
    }
}

async fn handle_manifest_changed<P, SessionRt, TraceRt, StoreRt, McpRt>(
    harness: &Harness<P, SessionRt, TraceRt, StoreRt, McpRt>,
    resource_type: ResourceType,
    resource_id: Uuid,
    action: ResourceAction,
    project_id: Option<Uuid>,
    payload: Option<serde_json::Value>,
    encrypted_payload: Option<EncryptedPayload>,
) -> Result<()>
where
    P: HarnessProvider,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
    TraceRt: ExecutionTraceRuntime + 'static,
    StoreRt: crate::handlers::manifest::ManifestStore + 'static,
    McpRt: crate::handlers::manifest::McpRuntime + 'static,
{
    let runtime = harness
        .manifest_services()
        .ok_or(HarnessError::ManifestServicesNotConfigured)?;
    let result = apply_manifest_change(
        runtime.client.as_ref(),
        runtime.store.as_ref(),
        runtime.mcp.as_deref(),
        harness.provider().manifest(),
        ManifestChange {
            resource_type,
            resource_id,
            action,
            project_id,
            payload,
            encrypted_payload,
        },
    )
    .await
    .map_err(HarnessError::manifest_runtime)?;

    harness.swap_provider(harness.provider().with_manifest(result.manifest));

    if should_refresh_domain_sessions(resource_type) {
        refresh_active_domain_sessions(harness).await;
    }

    Ok(())
}

fn should_refresh_domain_sessions(resource_type: ResourceType) -> bool {
    matches!(
        resource_type,
        ResourceType::Agent
            | ResourceType::Ability
            | ResourceType::Domain
            | ResourceType::McpServer
    )
}

async fn refresh_active_domain_sessions<P, SessionRt, TraceRt, StoreRt, McpRt>(
    harness: &Harness<P, SessionRt, TraceRt, StoreRt, McpRt>,
) where
    P: HarnessProvider,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
    TraceRt: ExecutionTraceRuntime + 'static,
    StoreRt: crate::handlers::manifest::ManifestStore + 'static,
    McpRt: crate::handlers::manifest::McpRuntime + 'static,
{
    let domains = harness.domains();
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
        match harness
            .rebuild_domain_session(session_id, agent_id, project_id, &domain_command)
            .await
        {
            Ok(session) => {
                domains.insert(session_id, session);
                info!(%session_id, %agent_id, domain = %domain_command, "Refreshed active domain session after manifest change");
            }
            Err(error) => {
                warn!(
                    %session_id,
                    %agent_id,
                    domain = %domain_command,
                    error = %error,
                    "Failed to refresh active domain session after manifest change"
                );
            }
        }
    }
}
