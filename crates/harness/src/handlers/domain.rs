//! Domain session handlers.

use anyhow::Result;
use tracing::{info, warn};
use uuid::Uuid;

use nenjo_events::{Response, StreamEvent};

use super::event_bridge::agent_name;
use crate::harness::{CommandContext, DomainSession};

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
                    turn_number: 0,
                },
            );

            info!(agent = %aname, domain = %domain_name, %session_id, "Domain expansion");

            let _ = ctx.response_tx.send(Response::AgentResponse {
                payload: StreamEvent::DomainEntered {
                    session_id,
                    domain_name,
                },
            });
        }
        Err(e) => {
            warn!(agent = %aname, error = %e, "Domain expansion failed");
            let _ = ctx.response_tx.send(Response::AgentResponse {
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
