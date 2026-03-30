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
    let base_runner = provider.agent_by_id(agent_id).await?.build()?;

    match base_runner.domain_expansion(domain_command) {
        Ok(domain_runner) => {
            let active = domain_runner
                .instance()
                .prompt_context
                .active_domain
                .as_ref();

            let domain_name = active
                .map(|d| d.domain_name.clone())
                .unwrap_or_else(|| domain_command.to_string());

            // Check if the domain escalates scopes — if so, rebuild through
            // the Provider so the ToolFactory adds MCP tools for the new scopes.
            let final_runner = if let Some(ref ad) =
                domain_runner.instance().prompt_context.active_domain
            {
                let tool_config = &ad.manifest.tools;

                if !tool_config.additional_scopes.is_empty() || !tool_config.activate_mcp.is_empty()
                {
                    // Rebuild the agent with escalated scopes
                    let mut expanded_instance = domain_runner.instance().clone();

                    // Merge additional_scopes into the agent's platform_scopes
                    for scope in &tool_config.additional_scopes {
                        if !expanded_instance
                            .prompt_context
                            .platform_scopes
                            .contains(scope)
                        {
                            expanded_instance
                                .prompt_context
                                .platform_scopes
                                .push(scope.clone());
                        }
                    }

                    // Rebuild tools through the Provider's ToolFactory with expanded scopes.
                    let mut agent_manifest = manifest
                        .agents
                        .iter()
                        .find(|a| a.id == agent_id)
                        .cloned()
                        .expect("agent must exist");
                    agent_manifest.platform_scopes =
                        expanded_instance.prompt_context.platform_scopes.clone();

                    let new_tools = provider.tool_factory().create_tools(&agent_manifest).await;

                    // Merge: keep domain-filtered tools + add new MCP tools
                    for tool in new_tools {
                        let name = tool.name().to_string();
                        if !expanded_instance.tools.iter().any(|t| t.name() == name) {
                            expanded_instance.tools.push(tool);
                        }
                    }

                    nenjo::AgentRunner::from_instance(expanded_instance)
                } else {
                    domain_runner
                }
            } else {
                domain_runner
            };

            ctx.domains.insert(
                session_id,
                DomainSession {
                    runner: final_runner,
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
