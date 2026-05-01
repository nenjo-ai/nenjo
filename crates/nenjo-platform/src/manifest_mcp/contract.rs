use anyhow::{Result, anyhow};
use serde::Serialize;

use nenjo::ToolSpec;

use super::backend::ManifestMcpBackend;
use super::params::{
    AbilitiesGetParams, AbilityCreateParams, AbilityDeleteParams, AbilityPromptGetParams,
    AbilityPromptUpdateParams, AbilityUpdateParams, AgentCreateParams, AgentDeleteParams,
    AgentPromptGetParams, AgentPromptUpdateParams, AgentUpdateParams, AgentsGetParams,
    ContextBlockContentGetParams, ContextBlockContentUpdateParams, ContextBlockCreateParams,
    ContextBlockDeleteParams, ContextBlockUpdateParams, ContextBlocksGetParams,
    CouncilAddMemberParams, CouncilCreateParams, CouncilDeleteParams, CouncilRemoveMemberParams,
    CouncilUpdateMemberParams, CouncilUpdateParams, CouncilsGetParams, DomainCreateParams,
    DomainDeleteParams, DomainPromptGetParams, DomainPromptUpdateParams, DomainUpdateParams,
    DomainsGetParams, ModelCreateParams, ModelDeleteParams, ModelUpdateParams, ModelsGetParams,
    ProjectCreateParams, ProjectDeleteParams, ProjectDocumentContentGetParams,
    ProjectDocumentContentUpdateParams, ProjectDocumentCreateParams, ProjectDocumentDeleteParams,
    ProjectDocumentGetParams, ProjectDocumentsListParams, ProjectUpdateParams, ProjectsGetParams,
    RoutineCreateParams, RoutineDeleteParams, RoutineUpdateParams, RoutinesGetParams,
};
use super::tools::all_tools;

/// Static manifest MCP tool registry and dispatcher.
pub struct ManifestMcpContract;

impl ManifestMcpContract {
    /// Return the canonical manifest MCP tool definitions.
    pub fn tools() -> Vec<ToolSpec> {
        all_tools()
    }

    /// Decode parameters, invoke the matching backend method, and serialize the result.
    pub async fn dispatch<B: ManifestMcpBackend + ?Sized>(
        backend: &B,
        tool_name: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value> {
        match tool_name {
            "list_agents" => {
                if !params.is_null() {
                    let object = params.as_object().cloned().unwrap_or_default();
                    if !object.is_empty() {
                        return Err(anyhow!("list_agents does not accept parameters"));
                    }
                }
                to_json(backend.agents_list().await?)
            }
            "get_agent" => {
                let args: AgentsGetParams = serde_json::from_value(params)?;
                to_json(backend.agents_get(args).await?)
            }
            "get_agent_prompt" => {
                let args: AgentPromptGetParams = serde_json::from_value(params)?;
                to_json(backend.agents_get_prompt(args).await?)
            }
            "create_agent" => {
                let args: AgentCreateParams = serde_json::from_value(params)?;
                to_json(backend.agents_create(args).await?)
            }
            "update_agent" => {
                let args: AgentUpdateParams = serde_json::from_value(params)?;
                to_json(backend.agents_update(args).await?)
            }
            "update_agent_prompt" => {
                let args: AgentPromptUpdateParams = serde_json::from_value(params)?;
                to_json(backend.agents_update_prompt(args).await?)
            }
            "delete_agent" => {
                let args: AgentDeleteParams = serde_json::from_value(params)?;
                to_json(backend.agents_delete(args).await?)
            }
            "list_abilities" => {
                if !params.is_null() {
                    let object = params.as_object().cloned().unwrap_or_default();
                    if !object.is_empty() {
                        return Err(anyhow!("list_abilities does not accept parameters"));
                    }
                }
                to_json(backend.abilities_list().await?)
            }
            "get_ability" => {
                let args: AbilitiesGetParams = serde_json::from_value(params)?;
                to_json(backend.abilities_get(args).await?)
            }
            "get_ability_prompt" => {
                let args: AbilityPromptGetParams = serde_json::from_value(params)?;
                to_json(backend.abilities_get_prompt(args).await?)
            }
            "create_ability" => {
                let args: AbilityCreateParams = serde_json::from_value(params)?;
                to_json(backend.abilities_create(args).await?)
            }
            "update_ability" => {
                let args: AbilityUpdateParams = serde_json::from_value(params)?;
                to_json(backend.abilities_update(args).await?)
            }
            "update_ability_prompt" => {
                let args: AbilityPromptUpdateParams = serde_json::from_value(params)?;
                to_json(backend.abilities_update_prompt(args).await?)
            }
            "delete_ability" => {
                let args: AbilityDeleteParams = serde_json::from_value(params)?;
                to_json(backend.abilities_delete(args).await?)
            }
            "list_domains" => {
                if !params.is_null() {
                    let object = params.as_object().cloned().unwrap_or_default();
                    if !object.is_empty() {
                        return Err(anyhow!("list_domains does not accept parameters"));
                    }
                }
                to_json(backend.domains_list().await?)
            }
            "get_domain" => {
                let args: DomainsGetParams = serde_json::from_value(params)?;
                to_json(backend.domains_get(args).await?)
            }
            "get_domain_prompt" => {
                let args: DomainPromptGetParams = serde_json::from_value(params)?;
                to_json(backend.domains_get_manifest(args).await?)
            }
            "create_domain" => {
                let args: DomainCreateParams = serde_json::from_value(params)?;
                to_json(backend.domains_create(args).await?)
            }
            "update_domain" => {
                let args: DomainUpdateParams = serde_json::from_value(params)?;
                to_json(backend.domains_update(args).await?)
            }
            "update_domain_prompt" => {
                let args: DomainPromptUpdateParams = serde_json::from_value(params)?;
                to_json(backend.domains_update_manifest(args).await?)
            }
            "delete_domain" => {
                let args: DomainDeleteParams = serde_json::from_value(params)?;
                to_json(backend.domains_delete(args).await?)
            }
            "list_projects" => {
                if !params.is_null() {
                    let object = params.as_object().cloned().unwrap_or_default();
                    if !object.is_empty() {
                        return Err(anyhow!("list_projects does not accept parameters"));
                    }
                }
                to_json(backend.projects_list().await?)
            }
            "get_project" => {
                let args: ProjectsGetParams = serde_json::from_value(params)?;
                to_json(backend.projects_get(args).await?)
            }
            "create_project" => {
                let args: ProjectCreateParams = serde_json::from_value(params)?;
                to_json(backend.projects_create(args).await?)
            }
            "update_project" => {
                let args: ProjectUpdateParams = serde_json::from_value(params)?;
                to_json(backend.projects_update(args).await?)
            }
            "delete_project" => {
                let args: ProjectDeleteParams = serde_json::from_value(params)?;
                to_json(backend.projects_delete(args).await?)
            }
            "list_project_documents" => {
                let args: ProjectDocumentsListParams = serde_json::from_value(params)?;
                to_json(backend.project_documents_list(args).await?)
            }
            "get_project_document" => {
                let args: ProjectDocumentGetParams = serde_json::from_value(params)?;
                to_json(backend.project_documents_get(args).await?)
            }
            "get_project_document_content" => {
                let args: ProjectDocumentContentGetParams = serde_json::from_value(params)?;
                to_json(backend.project_documents_get_content(args).await?)
            }
            "create_project_document" => {
                let args: ProjectDocumentCreateParams = serde_json::from_value(params)?;
                to_json(backend.project_documents_create(args).await?)
            }
            "update_project_document_content" => {
                let args: ProjectDocumentContentUpdateParams = serde_json::from_value(params)?;
                to_json(backend.project_documents_update_content(args).await?)
            }
            "delete_project_document" => {
                let args: ProjectDocumentDeleteParams = serde_json::from_value(params)?;
                to_json(backend.project_documents_delete(args).await?)
            }
            "list_routines" => {
                if !params.is_null() {
                    let object = params.as_object().cloned().unwrap_or_default();
                    if !object.is_empty() {
                        return Err(anyhow!("list_routines does not accept parameters"));
                    }
                }
                to_json(backend.routines_list().await?)
            }
            "get_routine" => {
                let args: RoutinesGetParams = serde_json::from_value(params)?;
                to_json(backend.routines_get(args).await?)
            }
            "create_routine" => {
                let args: RoutineCreateParams = serde_json::from_value(params)?;
                to_json(backend.routines_create(args).await?)
            }
            "update_routine" => {
                let args: RoutineUpdateParams = serde_json::from_value(params)?;
                to_json(backend.routines_update(args).await?)
            }
            "delete_routine" => {
                let args: RoutineDeleteParams = serde_json::from_value(params)?;
                to_json(backend.routines_delete(args).await?)
            }
            "list_models" => {
                if !params.is_null() {
                    let object = params.as_object().cloned().unwrap_or_default();
                    if !object.is_empty() {
                        return Err(anyhow!("list_models does not accept parameters"));
                    }
                }
                to_json(backend.models_list().await?)
            }
            "get_model" => {
                let args: ModelsGetParams = serde_json::from_value(params)?;
                to_json(backend.models_get(args).await?)
            }
            "create_model" => {
                let args: ModelCreateParams = serde_json::from_value(params)?;
                to_json(backend.models_create(args).await?)
            }
            "update_model" => {
                let args: ModelUpdateParams = serde_json::from_value(params)?;
                to_json(backend.models_update(args).await?)
            }
            "delete_model" => {
                let args: ModelDeleteParams = serde_json::from_value(params)?;
                to_json(backend.models_delete(args).await?)
            }
            "list_councils" => {
                if !params.is_null() {
                    let object = params.as_object().cloned().unwrap_or_default();
                    if !object.is_empty() {
                        return Err(anyhow!("list_councils does not accept parameters"));
                    }
                }
                to_json(backend.councils_list().await?)
            }
            "get_council" => {
                let args: CouncilsGetParams = serde_json::from_value(params)?;
                to_json(backend.councils_get(args).await?)
            }
            "create_council" => {
                let args: CouncilCreateParams = serde_json::from_value(params)?;
                to_json(backend.councils_create(args).await?)
            }
            "update_council" => {
                let args: CouncilUpdateParams = serde_json::from_value(params)?;
                to_json(backend.councils_update(args).await?)
            }
            "add_council_member" => {
                let args: CouncilAddMemberParams = serde_json::from_value(params)?;
                to_json(backend.councils_add_member(args).await?)
            }
            "update_council_member" => {
                let args: CouncilUpdateMemberParams = serde_json::from_value(params)?;
                to_json(backend.councils_update_member(args).await?)
            }
            "remove_council_member" => {
                let args: CouncilRemoveMemberParams = serde_json::from_value(params)?;
                to_json(backend.councils_remove_member(args).await?)
            }
            "delete_council" => {
                let args: CouncilDeleteParams = serde_json::from_value(params)?;
                to_json(backend.councils_delete(args).await?)
            }
            "list_context_blocks" => {
                if !params.is_null() {
                    let object = params.as_object().cloned().unwrap_or_default();
                    if !object.is_empty() {
                        return Err(anyhow!("list_context_blocks does not accept parameters"));
                    }
                }
                to_json(backend.context_blocks_list().await?)
            }
            "get_context_block" => {
                let args: ContextBlocksGetParams = serde_json::from_value(params)?;
                to_json(backend.context_blocks_get(args).await?)
            }
            "get_context_block_content" => {
                let args: ContextBlockContentGetParams = serde_json::from_value(params)?;
                to_json(backend.context_blocks_get_content(args).await?)
            }
            "create_context_block" => {
                let args: ContextBlockCreateParams = serde_json::from_value(params)?;
                to_json(backend.context_blocks_create(args).await?)
            }
            "update_context_block" => {
                let args: ContextBlockUpdateParams = serde_json::from_value(params)?;
                to_json(backend.context_blocks_update(args).await?)
            }
            "update_context_block_content" => {
                let args: ContextBlockContentUpdateParams = serde_json::from_value(params)?;
                to_json(backend.context_blocks_update_content(args).await?)
            }
            "delete_context_block" => {
                let args: ContextBlockDeleteParams = serde_json::from_value(params)?;
                to_json(backend.context_blocks_delete(args).await?)
            }
            other => Err(anyhow!("unknown manifest MCP tool: {other}")),
        }
    }
}
fn to_json<T: Serialize>(value: T) -> Result<serde_json::Value> {
    Ok(serde_json::to_value(value)?)
}
