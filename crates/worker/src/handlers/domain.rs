//! Domain command integration.

use anyhow::Result;
use nenjo_sessions::{
    SessionStatus, SessionTranscriptAppend, SessionTranscriptEventPayload, SessionTransition,
    TranscriptState,
};
use tracing::{info, warn};
use uuid::Uuid;

use nenjo::Slug;
use nenjo_harness::{Harness, ProviderRuntime};

use crate::event_bridge::agent_name;
use crate::resource_resolver::PlatformResourceResolver;

#[derive(Clone)]
pub struct DomainCommandContext {
    pub worker_id: String,
}

#[async_trait::async_trait]
/// Worker integration methods for domain-session platform commands.
///
/// Domain command handling coordinates the in-memory domain registry with
/// persisted session/transcript state while leaving the core harness domain
/// rebuild logic platform-neutral.
pub(crate) trait WorkerDomainHarnessExt {
    /// Exit an active domain session and append the appropriate chat transcript
    /// marker when a parent chat session is known.
    async fn handle_domain_exit(
        &self,
        ctx: &DomainCommandContext,
        agent: &str,
        domain_session_id: Uuid,
        chat_session_id: Option<Uuid>,
    ) -> Result<()>;
}

#[async_trait::async_trait]
impl<P, SessionRt> WorkerDomainHarnessExt for Harness<P, SessionRt>
where
    P: ProviderRuntime,
    SessionRt: nenjo_sessions::SessionRuntime + 'static,
{
    /// Exit a domain session by removing the stored runner.
    async fn handle_domain_exit(
        &self,
        ctx: &DomainCommandContext,
        agent: &str,
        domain_session_id: Uuid,
        chat_session_id: Option<Uuid>,
    ) -> Result<()> {
        let provider = self.provider();
        let manifest = provider.manifest_snapshot();
        let agent_slug = Slug::parse(agent)?;
        let agent_id = PlatformResourceResolver::new(&manifest).agent_id(&agent_slug)?;
        let aname = agent_name(&manifest, agent_id);

        let session = self.domains().remove(&domain_session_id).map(|(_, v)| v);
        let _ = self
            .sessions()
            .transition(SessionTransition {
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
                        .sessions()
                        .append_transcript(SessionTranscriptAppend {
                            session_id: chat_session_id,
                            turn_id: None,
                            payload: SessionTranscriptEventPayload::DomainDeactivated {
                                domain_session_id,
                                domain_command: session.domain_command.clone(),
                                domain_name: domain_name.clone(),
                                agent_id: Some(agent_id),
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
