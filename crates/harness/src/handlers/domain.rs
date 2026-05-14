//! Domain session handlers.

use anyhow::Result;
use nenjo::memory::MemoryScope;
use nenjo_events::{Response, StreamEvent};
use nenjo_sessions::{DomainSessionUpsert, DomainState, SessionStatus, SessionTransition};
use tracing::{info, warn};
use uuid::Uuid;

use super::ResponseSender;
use crate::event_bridge::{agent_name, project_slug};
use crate::execution_trace::ExecutionTraceRuntime;
use crate::{DomainSession, Harness, HarnessProvider};

#[derive(Clone)]
pub struct DomainCommandContext<S> {
    pub response_sink: S,
    pub worker_id: String,
}

fn domain_memory_namespace(agent_name: &str, project_slug: &str) -> String {
    MemoryScope::new(
        agent_name,
        if project_slug.is_empty() {
            None
        } else {
            Some(project_slug)
        },
    )
    .project
}

impl<P, SessionRt, TraceRt, StoreRt, McpRt> Harness<P, SessionRt, TraceRt, StoreRt, McpRt>
where
    P: HarnessProvider,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
    TraceRt: ExecutionTraceRuntime + 'static,
    StoreRt: crate::handlers::manifest::ManifestStore + 'static,
    McpRt: crate::handlers::manifest::McpRuntime + 'static,
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
            if let Some(project) = provider
                .manifest()
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

    /// Enter a domain session by creating a domain-expanded runner.
    pub async fn handle_domain_enter<S>(
        &self,
        ctx: &DomainCommandContext<S>,
        project_id: Uuid,
        agent_id: Uuid,
        domain_command: &str,
        session_id: Uuid,
    ) -> Result<()>
    where
        S: ResponseSender,
    {
        let provider = self.provider();
        let manifest = provider.manifest();
        let aname = agent_name(manifest, agent_id);
        let pslug = project_slug(manifest, project_id);

        let mut builder = provider.build_agent_by_id(agent_id).await?;
        if !project_id.is_nil() {
            if let Some(project) = manifest
                .projects
                .iter()
                .find(|project| project.id == project_id)
            {
                builder = builder.with_project_context(project);
            } else {
                warn!(%project_id, %agent_id, "Project not found in manifest for domain session");
            }
        }
        let base_runner = builder.build().await?;

        match base_runner.domain_expansion(domain_command).await {
            Ok(domain_runner) => {
                let domain_name = domain_runner
                    .instance()
                    .prompt_context()
                    .active_domain
                    .as_ref()
                    .map(|d| d.domain_name.clone())
                    .unwrap_or_else(|| domain_command.to_string());

                self.domains().insert(
                    session_id,
                    DomainSession {
                        runner: domain_runner,
                        agent_id,
                        project_id,
                        domain_command: domain_command.to_string(),
                    },
                );
                let _ = self
                    .upsert_domain_session(DomainSessionUpsert {
                        session_id,
                        status: SessionStatus::Active,
                        project_id: if project_id.is_nil() {
                            None
                        } else {
                            Some(project_id)
                        },
                        agent_id,
                        worker_id: ctx.worker_id.clone(),
                        memory_namespace: Some(domain_memory_namespace(&aname, &pslug)),
                        domain: Some(DomainState {
                            domain_command: domain_command.to_string(),
                        }),
                    })
                    .await;

                info!(
                    agent = %aname,
                    agent_id = %agent_id,
                    domain = %domain_name,
                    %session_id,
                    "Domain session entered"
                );

                let _ = ctx.response_sink.send(Response::AgentResponse {
                    session_id: None,
                    payload: StreamEvent::DomainEntered {
                        session_id,
                        domain_name,
                    },
                });
            }
            Err(error) => {
                warn!(agent = %aname, error = %error, "Domain expansion failed");
                let _ = ctx.response_sink.send(Response::AgentResponse {
                    session_id: None,
                    payload: StreamEvent::Error {
                        message: format!("Domain expansion failed: {error}"),
                        payload: None,
                        encrypted_payload: None,
                    },
                });
            }
        }

        Ok(())
    }

    /// Exit a domain session by removing the stored runner.
    pub async fn handle_domain_exit<S>(
        &self,
        ctx: &DomainCommandContext<S>,
        agent_id: Uuid,
        domain_session_id: Uuid,
    ) -> Result<()>
    where
        S: ResponseSender,
    {
        let provider = self.provider();
        let manifest = provider.manifest();
        let aname = agent_name(manifest, agent_id);

        let session = self.domains().remove(&domain_session_id).map(|(_, v)| v);
        let _ = self
            .transition_session(SessionTransition {
                session_id: domain_session_id,
                worker_id: ctx.worker_id.clone(),
                phase: None,
                status: SessionStatus::Completed,
            })
            .await;

        match session {
            Some(session) => {
                let domain_name = session
                    .runner
                    .instance()
                    .prompt_context()
                    .active_domain
                    .as_ref()
                    .map(|d| d.domain_name.clone())
                    .unwrap_or_default();

                info!(
                    agent = %aname,
                    domain = %domain_name,
                    %domain_session_id,
                    "Domain session ended"
                );

                let _ = ctx.response_sink.send(Response::AgentResponse {
                    session_id: None,
                    payload: StreamEvent::DomainExited {
                        session_id: domain_session_id,
                        artifact_id: None,
                        document_id: None,
                    },
                });
            }
            None => {
                warn!(%domain_session_id, "Domain exit for unknown session");
                let _ = ctx.response_sink.send(Response::AgentResponse {
                    session_id: None,
                    payload: StreamEvent::DomainExited {
                        session_id: domain_session_id,
                        artifact_id: None,
                        document_id: None,
                    },
                });
            }
        }

        Ok(())
    }
}
