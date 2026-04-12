//! Domain session handlers.

use anyhow::Result;
use chrono::Utc;
use nenjo::memory::MemoryScope;
use nenjo_sessions::{
    DomainState, SessionKind, SessionRecord, SessionRefs, SessionStatus, SessionSummary,
};
use tracing::{info, warn};
use uuid::Uuid;

use nenjo_events::{Response, StreamEvent};

use super::event_bridge::agent_name;
use crate::harness::session::{lease_for_status, update_session_status};
use crate::harness::{CommandContext, DomainSession};

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

/// Enter a domain session — creates a domain-expanded runner with escalated scopes.
///
/// The domain's `additional_scopes` are merged into the agent's `platform_scopes`
/// before rebuilding through the Provider, so the `ToolFactory` sees the expanded
/// scopes and includes the corresponding MCP tools.
pub async fn handle_domain_enter(
    ctx: &CommandContext,
    project_id: Uuid,
    agent_id: Uuid,
    domain_command: &str,
    session_id: Uuid,
) -> Result<()> {
    let provider = ctx.provider();
    let manifest = provider.manifest();
    let aname = agent_name(manifest, agent_id);
    let pslug = super::event_bridge::project_slug(manifest, project_id);

    // First do the domain expansion to get the session config + filtered tools.
    let base_runner = provider.agent_by_id(agent_id).await?.build().await?;

    match base_runner.domain_expansion(domain_command).await {
        Ok(domain_runner) => {
            let domain_name = domain_runner
                .instance()
                .prompt_context
                .active_domain
                .as_ref()
                .map(|d| d.domain_name.clone())
                .unwrap_or_else(|| domain_command.to_string());

            ctx.domains.insert(
                session_id,
                DomainSession {
                    runner: domain_runner,
                    agent_id,
                    project_id,
                    domain_command: domain_command.to_string(),
                    turn_number: 0,
                },
            );
            let now = Utc::now();
            let _ = ctx.session_store.put(&SessionRecord {
                session_id,
                kind: SessionKind::Domain,
                status: SessionStatus::Active,
                project_id: Some(project_id),
                agent_id: Some(agent_id),
                task_id: None,
                routine_id: None,
                execution_run_id: None,
                parent_session_id: None,
                version: 1,
                refs: SessionRefs {
                    memory_namespace: Some(domain_memory_namespace(&aname, &pslug)),
                    ..SessionRefs::default()
                },
                lease: lease_for_status(
                    &*ctx.session_coordinator,
                    session_id,
                    &ctx.worker_id,
                    SessionStatus::Active,
                    &nenjo_sessions::SessionLease::default(),
                ),
                scheduler: None,
                domain: Some(DomainState {
                    domain_command: domain_command.to_string(),
                    turn_number: 0,
                }),
                summary: SessionSummary::default(),
                created_at: now,
                updated_at: now,
                completed_at: None,
            });

            info!(
                agent = %aname,
                agent_id = %agent_id,
                domain = %domain_name,
                %session_id,
                "Domain session entered"
            );

            let _ = ctx.response_tx.send(Response::AgentResponse {
                session_id: None,
                payload: StreamEvent::DomainEntered {
                    session_id,
                    domain_name,
                },
            });
        }
        Err(e) => {
            warn!(agent = %aname, error = %e, "Domain expansion failed");
            let _ = ctx.response_tx.send(Response::AgentResponse {
                session_id: None,
                payload: StreamEvent::Error {
                    message: format!("Domain expansion failed: {e}"),
                },
            });
        }
    }

    Ok(())
}

/// Exit a domain session — removes the stored runner.
pub async fn handle_domain_exit(
    ctx: &CommandContext,
    _project_id: Uuid,
    agent_id: Uuid,
    domain_session_id: Uuid,
) -> Result<()> {
    let provider = ctx.provider();
    let manifest = provider.manifest();
    let aname = agent_name(manifest, agent_id);

    let session = ctx.domains.remove(&domain_session_id).map(|(_, v)| v);
    let _ = update_session_status(
        &*ctx.session_store,
        &*ctx.session_coordinator,
        domain_session_id,
        &ctx.worker_id,
        SessionStatus::Completed,
    );

    match session {
        Some(session) => {
            let domain_name = session
                .runner
                .instance()
                .prompt_context
                .active_domain
                .as_ref()
                .map(|d| d.domain_name.clone())
                .unwrap_or_default();

            info!(
                agent = %aname,
                domain = %domain_name,
                %domain_session_id,
                turns = session.turn_number,
                "Domain session ended"
            );

            let _ = ctx.response_tx.send(Response::AgentResponse {
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
            let _ = ctx.response_tx.send(Response::AgentResponse {
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
