//! Domain session runtime helpers.

use std::sync::Arc;

use anyhow::Result;
use dashmap::DashMap;
use tracing::warn;
use uuid::Uuid;

use crate::{Harness, ProviderRuntime};

/// An active domain session holding the domain-expanded runner and state.
pub struct DomainSession<P: ProviderRuntime = nenjo::provider::ErasedProvider> {
    pub runner: nenjo::AgentRunner<P>,
    pub agent_id: Uuid,
    pub project_id: Uuid,
    pub domain_command: String,
}

/// Thread-safe registry of active domain sessions, keyed by domain session id.
pub type DomainRegistry<P = nenjo::provider::ErasedProvider> = Arc<DashMap<Uuid, DomainSession<P>>>;

impl<P, SessionRt> Harness<P, SessionRt>
where
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
{
    /// Rebuild a persisted domain session against the current provider snapshot.
    pub async fn rebuild_domain_session(
        &self,
        session_id: Uuid,
        agent_id: Uuid,
        project_id: Uuid,
        domain_command: &str,
    ) -> Result<DomainSession<P>> {
        let provider = self.provider();
        let mut builder = provider.build_agent_by_id(agent_id).await?;
        if !project_id.is_nil() {
            let manifest = provider.manifest_snapshot();
            if let Some(project) = manifest
                .projects
                .iter()
                .find(|project| project.id == project_id)
            {
                builder = builder.with_project_context(project);
            } else {
                warn!(%project_id, %agent_id, "Project not found in manifest for domain session rebuild");
            }
        }
        let base_runner = builder.build().await?;
        let domain_runner = base_runner.domain_expansion(domain_command).await?;

        let mut instance = domain_runner.instance().clone();
        instance.set_active_domain_session_id(session_id);

        let runner = nenjo::AgentRunner::from_instance(
            instance,
            domain_runner.memory().cloned(),
            domain_runner.memory_scope().cloned(),
        );

        Ok(DomainSession {
            runner,
            agent_id,
            project_id,
            domain_command: domain_command.to_string(),
        })
    }
}
