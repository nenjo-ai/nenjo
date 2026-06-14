use anyhow::{Result, anyhow};
use serde::Serialize;

use nenjo::ToolSpec;

use super::backend::ManifestMcpBackend;
use super::params::{
    AbilitiesGetParams, AbilityConfigureParams, AgentConfigureParams, AgentsGetParams,
    ContextBlockConfigureParams, ContextBlocksGetParams, CouncilAddMemberParams,
    CouncilCreateParams, CouncilDeleteParams, CouncilRemoveMemberParams, CouncilUpdateMemberParams,
    CouncilUpdateParams, CouncilsGetParams, DomainConfigureParams, DomainsGetParams,
    KnowledgeDocCreateParams, KnowledgeDocDeleteParams, KnowledgeDocUpdateParams,
    KnowledgePackCreateParams, KnowledgePackUpdateParams, ModelCreateParams, ModelDeleteParams,
    ModelUpdateParams, ModelsGetParams, ProjectCreateParams, ProjectDeleteParams,
    ProjectUpdateParams, ProjectsGetParams, RoutineConfigureParams, RoutinesGetParams,
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
                to_json(backend.list_agents().await?)
            }
            "get_agent" => {
                let args: AgentsGetParams = serde_json::from_value(params)?;
                to_json(backend.get_agent(args).await?)
            }
            "configure_agent" => {
                let args: AgentConfigureParams = serde_json::from_value(params)?;
                to_json(backend.configure_agent(args).await?)
            }
            "list_abilities" => {
                if !params.is_null() {
                    let object = params.as_object().cloned().unwrap_or_default();
                    if !object.is_empty() {
                        return Err(anyhow!("list_abilities does not accept parameters"));
                    }
                }
                to_json(backend.list_abilities().await?)
            }
            "get_ability" => {
                let args: AbilitiesGetParams = serde_json::from_value(params)?;
                to_json(backend.get_ability(args).await?)
            }
            "configure_ability" => {
                let args: AbilityConfigureParams = serde_json::from_value(params)?;
                to_json(backend.configure_ability(args).await?)
            }
            "list_domains" => {
                if !params.is_null() {
                    let object = params.as_object().cloned().unwrap_or_default();
                    if !object.is_empty() {
                        return Err(anyhow!("list_domains does not accept parameters"));
                    }
                }
                to_json(backend.list_domains().await?)
            }
            "get_domain" => {
                let args: DomainsGetParams = serde_json::from_value(params)?;
                to_json(backend.get_domain(args).await?)
            }
            "configure_domain" => {
                let args: DomainConfigureParams = serde_json::from_value(params)?;
                to_json(backend.configure_domain(args).await?)
            }
            "list_projects" => {
                if !params.is_null() {
                    let object = params.as_object().cloned().unwrap_or_default();
                    if !object.is_empty() {
                        return Err(anyhow!("list_projects does not accept parameters"));
                    }
                }
                to_json(backend.list_projects().await?)
            }
            "get_project" => {
                let args: ProjectsGetParams = serde_json::from_value(params)?;
                to_json(backend.get_project(args).await?)
            }
            "create_project" => {
                let args: ProjectCreateParams = serde_json::from_value(params)?;
                to_json(backend.create_project(args).await?)
            }
            "update_project" => {
                let args: ProjectUpdateParams = serde_json::from_value(params)?;
                to_json(backend.update_project(args).await?)
            }
            "delete_project" => {
                let args: ProjectDeleteParams = serde_json::from_value(params)?;
                to_json(backend.delete_project(args).await?)
            }
            "create_knowledge_pack" => {
                let args: KnowledgePackCreateParams = serde_json::from_value(params)?;
                to_json(backend.create_knowledge_pack(args).await?)
            }
            "update_knowledge_pack" => {
                let args: KnowledgePackUpdateParams = serde_json::from_value(params)?;
                to_json(backend.update_knowledge_pack(args).await?)
            }
            "create_knowledge_doc" => {
                let args: KnowledgeDocCreateParams = serde_json::from_value(params)?;
                to_json(backend.create_knowledge_doc(args).await?)
            }
            "update_knowledge_doc" => {
                let args: KnowledgeDocUpdateParams = serde_json::from_value(params)?;
                to_json(backend.update_knowledge_doc(args).await?)
            }
            "delete_knowledge_doc" => {
                let args: KnowledgeDocDeleteParams = serde_json::from_value(params)?;
                to_json(backend.delete_knowledge_doc(args).await?)
            }
            "list_routines" => {
                if !params.is_null() {
                    let object = params.as_object().cloned().unwrap_or_default();
                    if !object.is_empty() {
                        return Err(anyhow!("list_routines does not accept parameters"));
                    }
                }
                to_json(backend.list_routines().await?)
            }
            "get_routine" => {
                let args: RoutinesGetParams = serde_json::from_value(params)?;
                to_json(backend.get_routine(args).await?)
            }
            "configure_routine" => {
                let args: RoutineConfigureParams = serde_json::from_value(params)?;
                to_json(backend.configure_routine(args).await?)
            }
            "list_models" => {
                if !params.is_null() {
                    let object = params.as_object().cloned().unwrap_or_default();
                    if !object.is_empty() {
                        return Err(anyhow!("list_models does not accept parameters"));
                    }
                }
                to_json(backend.list_models().await?)
            }
            "get_model" => {
                let args: ModelsGetParams = serde_json::from_value(params)?;
                to_json(backend.get_model(args).await?)
            }
            "create_model" => {
                let args: ModelCreateParams = serde_json::from_value(params)?;
                to_json(backend.create_model(args).await?)
            }
            "update_model" => {
                let args: ModelUpdateParams = serde_json::from_value(params)?;
                to_json(backend.update_model(args).await?)
            }
            "delete_model" => {
                let args: ModelDeleteParams = serde_json::from_value(params)?;
                to_json(backend.delete_model(args).await?)
            }
            "list_councils" => {
                if !params.is_null() {
                    let object = params.as_object().cloned().unwrap_or_default();
                    if !object.is_empty() {
                        return Err(anyhow!("list_councils does not accept parameters"));
                    }
                }
                to_json(backend.list_councils().await?)
            }
            "get_council" => {
                let args: CouncilsGetParams = serde_json::from_value(params)?;
                to_json(backend.get_council(args).await?)
            }
            "create_council" => {
                let args: CouncilCreateParams = serde_json::from_value(params)?;
                to_json(backend.create_council(args).await?)
            }
            "update_council" => {
                let args: CouncilUpdateParams = serde_json::from_value(params)?;
                to_json(backend.update_council(args).await?)
            }
            "add_council_member" => {
                let args: CouncilAddMemberParams = serde_json::from_value(params)?;
                to_json(backend.add_council_member(args).await?)
            }
            "update_council_member" => {
                let args: CouncilUpdateMemberParams = serde_json::from_value(params)?;
                to_json(backend.update_council_member(args).await?)
            }
            "remove_council_member" => {
                let args: CouncilRemoveMemberParams = serde_json::from_value(params)?;
                to_json(backend.remove_council_member(args).await?)
            }
            "delete_council" => {
                let args: CouncilDeleteParams = serde_json::from_value(params)?;
                to_json(backend.delete_council(args).await?)
            }
            "list_context_blocks" => {
                if !params.is_null() {
                    let object = params.as_object().cloned().unwrap_or_default();
                    if !object.is_empty() {
                        return Err(anyhow!("list_context_blocks does not accept parameters"));
                    }
                }
                to_json(backend.list_context_blocks().await?)
            }
            "get_context_block" => {
                let args: ContextBlocksGetParams = serde_json::from_value(params)?;
                to_json(backend.get_context_block(args).await?)
            }
            "configure_context_block" => {
                let args: ContextBlockConfigureParams = serde_json::from_value(params)?;
                to_json(backend.configure_context_block(args).await?)
            }
            other => Err(anyhow!("unknown manifest MCP tool: {other}")),
        }
    }
}
fn to_json<T: Serialize>(value: T) -> Result<serde_json::Value> {
    Ok(serde_json::to_value(value)?)
}
