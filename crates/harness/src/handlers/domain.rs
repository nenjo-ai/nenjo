//! Domain session handlers.

use anyhow::Result;
use nenjo_sessions::{
    SessionStatus, SessionTranscriptAppend, SessionTranscriptEventPayload, SessionTransition,
    TranscriptState,
};
use tracing::{info, warn};
use uuid::Uuid;

use crate::event_bridge::agent_name;
use crate::execution_trace::ExecutionTraceRuntime;
use crate::{DomainSession, Harness, HarnessProvider};

#[derive(Clone)]
pub struct DomainCommandContext {
    pub worker_id: String,
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

    /// Exit a domain session by removing the stored runner.
    pub async fn handle_domain_exit(
        &self,
        ctx: &DomainCommandContext,
        agent_id: Uuid,
        domain_session_id: Uuid,
        chat_session_id: Option<Uuid>,
    ) -> Result<()> {
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
                    .unwrap_or_else(|| session.domain_command.clone());

                if let Some(chat_session_id) = chat_session_id {
                    let _ = self
                        .append_transcript(SessionTranscriptAppend {
                            session_id: chat_session_id,
                            turn_id: None,
                            payload: SessionTranscriptEventPayload::DomainDeactivated {
                                domain_session_id,
                                domain_command: session.domain_command.clone(),
                                domain_name: domain_name.clone(),
                                agent_id,
                            },
                            transcript_state: TranscriptState::Clean,
                        })
                        .await;
                }

                info!(
                    agent = %aname,
                    domain = %domain_name,
                    %domain_session_id,
                    "Domain session ended"
                );
            }
            None => {
                warn!(%domain_session_id, "Domain exit for unknown session");
            }
        }

        Ok(())
    }
}
