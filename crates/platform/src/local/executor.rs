//! In-process manifest MCP backend backed by a local manifest reader and writer.

use std::sync::Arc;

use anyhow::{Result, anyhow, bail};
use async_trait::async_trait;
use nenjo::{
    ManifestReader, ManifestResource, ManifestResourceKind, ManifestWriter,
    manifest::{
        AbilityManifest, AgentManifest, ContextBlockManifest, CouncilDelegationStrategy,
        CouncilManifest, CouncilMemberManifest, DomainManifest, ModelManifest, ProjectManifest,
        PromptConfig, RoutineEdgeManifest, RoutineManifest, RoutineMetadata, RoutineStepManifest,
        RoutineTrigger,
    },
};

use crate::manifest_mcp::{
    AbilitiesGetParams, AbilitiesListResult, AbilityDeleteParams, AbilityDocument,
    AbilityGetResult, AbilityManifestBackend, AbilityMutationResult, AbilityPromptDocument,
    AbilityPromptGetParams, AbilityPromptGetResult, AbilityPromptMutationResult,
    AbilityPromptUpdateParams, AbilitySummary, AbilityUpdateParams, AgentCreateParams,
    AgentDeleteParams, AgentDocument, AgentGetResult, AgentManifestBackend, AgentMutationResult,
    AgentPromptGetParams, AgentPromptGetResult, AgentPromptMutationResult, AgentPromptUpdateParams,
    AgentSummary, AgentsGetParams, AgentsListResult, ContextBlockContentDocument,
    ContextBlockContentGetParams, ContextBlockContentGetResult, ContextBlockContentMutationResult,
    ContextBlockContentUpdateParams, ContextBlockDeleteParams, ContextBlockDocument,
    ContextBlockGetResult, ContextBlockManifestBackend, ContextBlockMutationResult,
    ContextBlockUpdateParams, ContextBlocksGetParams, ContextBlocksListResult,
    CouncilAddMemberParams, CouncilDeleteParams, CouncilDocument, CouncilGetResult,
    CouncilManifestBackend, CouncilMutationResult, CouncilRemoveMemberParams,
    CouncilUpdateMemberParams, CouncilUpdateParams, CouncilsGetParams, CouncilsListResult,
    DeleteResult, DomainDeleteParams, DomainDocument, DomainGetResult, DomainManifestBackend,
    DomainManifestDocument, DomainManifestGetParams, DomainManifestGetResult,
    DomainManifestMutationResult, DomainManifestUpdateParams, DomainMutationResult, DomainSummary,
    DomainUpdateParams, DomainsGetParams, DomainsListResult, ModelDeleteParams, ModelDocument,
    ModelGetResult, ModelManifestBackend, ModelMutationResult, ModelUpdateParams, ModelsGetParams,
    ModelsListResult, ProjectDeleteParams, ProjectDocument, ProjectDocumentContentMutationResult,
    ProjectDocumentContentUpdateParams, ProjectDocumentMutationResult, ProjectGetResult,
    ProjectManifestBackend, ProjectMutationResult, ProjectSummary, ProjectUpdateParams,
    ProjectsGetParams, ProjectsListResult, RoutineDeleteParams, RoutineDocument, RoutineGetResult,
    RoutineGraphInput, RoutineManifestBackend, RoutineMutationResult, RoutineUpdateParams,
    RoutinesGetParams, RoutinesListResult,
};
use crate::{
    AbilityCreateParams, AgentUpdateParams, ContextBlockCreateParams, CouncilCreateParams,
    DomainCreateParams, ModelCreateParams, ProjectCreateParams, ProjectDocumentCreateParams,
    ProjectDocumentDeleteParams, RoutineCreateParams,
};

fn merge_json_patch(target: &mut serde_json::Value, patch: serde_json::Value) {
    match (target, patch) {
        (serde_json::Value::Object(target), serde_json::Value::Object(patch)) => {
            for (key, value) in patch {
                match target.get_mut(&key) {
                    Some(existing) => merge_json_patch(existing, value),
                    None => {
                        target.insert(key, value);
                    }
                }
            }
        }
        (target, patch) => *target = patch,
    }
}

fn merge_prompt_config(current: &PromptConfig, patch: serde_json::Value) -> Result<PromptConfig> {
    let mut value = serde_json::to_value(current)?;
    merge_json_patch(&mut value, patch);
    Ok(serde_json::from_value(value)?)
}

fn graph_input_to_manifest_parts(
    routine_id: uuid::Uuid,
    mut metadata: RoutineMetadata,
    graph: Option<RoutineGraphInput>,
) -> (
    Vec<RoutineStepManifest>,
    Vec<RoutineEdgeManifest>,
    RoutineMetadata,
) {
    let Some(graph) = graph else {
        return (Vec::new(), Vec::new(), metadata);
    };

    let step_ids: std::collections::HashMap<String, uuid::Uuid> = graph
        .steps
        .iter()
        .map(|step| {
            (
                step.step_id.clone(),
                uuid::Uuid::parse_str(&step.step_id).unwrap_or_else(|_| uuid::Uuid::new_v4()),
            )
        })
        .collect();

    metadata.entry_step_ids = graph
        .entry_step_ids
        .iter()
        .filter_map(|step_id| step_ids.get(step_id).copied())
        .collect();

    let steps = graph
        .steps
        .into_iter()
        .map(|step| RoutineStepManifest {
            id: *step_ids
                .get(&step.step_id)
                .expect("step id mapping should exist"),
            routine_id,
            name: step.name,
            step_type: step.step_type,
            council_id: step.council_id,
            agent_id: step.agent_id,
            config: step.config,
            order_index: step.order_index,
        })
        .collect();

    let edges = graph
        .edges
        .into_iter()
        .map(|edge| RoutineEdgeManifest {
            id: uuid::Uuid::new_v4(),
            routine_id,
            source_step_id: *step_ids
                .get(&edge.source_step_id)
                .expect("edge source mapping should exist"),
            target_step_id: *step_ids
                .get(&edge.target_step_id)
                .expect("edge target mapping should exist"),
            condition: edge.condition,
        })
        .collect();

    (steps, edges, metadata)
}

/// Manifest MCP backend that reads and writes directly against a local manifest store.
pub struct LocalManifestMcpBackend<R, W> {
    reader: Arc<R>,
    writer: Arc<W>,
}

impl<R, W> LocalManifestMcpBackend<R, W> {
    /// Create a backend from separate manifest reader and writer implementations.
    pub fn new(reader: Arc<R>, writer: Arc<W>) -> Self {
        Self { reader, writer }
    }
}

#[async_trait]
impl<R, W> AgentManifestBackend for LocalManifestMcpBackend<R, W>
where
    R: ManifestReader + Send + Sync,
    W: ManifestWriter + Send + Sync,
{
    async fn list_agents(&self) -> Result<AgentsListResult> {
        let agents: Vec<AgentSummary> = self
            .reader
            .list_agents()
            .await?
            .into_iter()
            .map(|agent| AgentDocument::from(agent).summary)
            .collect();
        Ok(AgentsListResult { agents })
    }

    async fn get_agent(&self, params: AgentsGetParams) -> Result<AgentGetResult> {
        let agent = self
            .reader
            .get_agent(params.id)
            .await?
            .ok_or_else(|| anyhow!("agent not found: {}", params.id))?;
        Ok(AgentGetResult {
            agent: AgentDocument::from(agent),
        })
    }

    async fn get_agent_prompt(&self, params: AgentPromptGetParams) -> Result<AgentPromptGetResult> {
        let agent = self
            .reader
            .get_agent(params.id)
            .await?
            .ok_or_else(|| anyhow!("agent not found: {}", params.id))?;
        Ok(AgentPromptGetResult {
            agent: agent.into(),
        })
    }

    async fn create_agent(&self, params: AgentCreateParams) -> Result<AgentMutationResult> {
        let agent = AgentManifest {
            id: uuid::Uuid::new_v4(),
            name: params.data.name,
            description: params.data.description,
            prompt_config: PromptConfig::default(),
            color: params.data.color,
            model_id: params.data.model_id,
            domain_ids: Vec::new(),
            platform_scopes: params.data.platform_scopes.unwrap_or_default(),
            mcp_server_ids: Vec::new(),
            ability_ids: Vec::new(),
            prompt_locked: false,
            heartbeat: None,
        };
        self.writer
            .upsert_resource(&ManifestResource::Agent(agent.clone()))
            .await?;
        Ok(AgentMutationResult {
            agent: AgentDocument::from(agent),
        })
    }

    async fn update_agent(&self, params: AgentUpdateParams) -> Result<AgentMutationResult> {
        let existing = self
            .reader
            .get_agent(params.id)
            .await?
            .ok_or_else(|| anyhow!("agent not found: {}", params.id))?;
        let mut agent: AgentManifest = existing.clone();
        if let Some(name) = params.data.name {
            agent.name = name;
        }
        if let Some(description) = params.data.description {
            agent.description = description;
        }
        if let Some(color) = params.data.color {
            agent.color = color;
        }
        if let Some(model_id) = params.data.model_id {
            agent.model_id = model_id;
        }
        if let Some(platform_scopes) = params.data.platform_scopes {
            agent.platform_scopes = platform_scopes;
        }
        let resource = ManifestResource::Agent(agent.clone());
        self.writer.upsert_resource(&resource).await?;
        Ok(AgentMutationResult {
            agent: AgentDocument::from(agent),
        })
    }

    async fn update_agent_prompt(
        &self,
        params: AgentPromptUpdateParams,
    ) -> Result<AgentPromptMutationResult> {
        let mut agent = self
            .reader
            .get_agent(params.id)
            .await?
            .ok_or_else(|| anyhow!("agent not found: {}", params.id))?;
        if agent.prompt_locked {
            return Err(anyhow!("agent prompt is locked: {}", params.id));
        }
        if let Some(prompt_patch) = params.prompt_config {
            agent.prompt_config = merge_prompt_config(&agent.prompt_config, prompt_patch)?;
        }
        let prompt_config = agent.prompt_config.clone();
        self.writer
            .upsert_resource(&ManifestResource::Agent(agent))
            .await?;
        Ok(AgentPromptMutationResult { prompt_config })
    }

    async fn delete_agent(&self, params: AgentDeleteParams) -> Result<DeleteResult> {
        self.writer
            .delete_resource(ManifestResourceKind::Agent, params.id)
            .await?;
        Ok(DeleteResult {
            deleted: true,
            id: params.id,
        })
    }
}

#[async_trait]
impl<R, W> AbilityManifestBackend for LocalManifestMcpBackend<R, W>
where
    R: ManifestReader + Send + Sync,
    W: ManifestWriter + Send + Sync,
{
    async fn list_abilities(&self) -> Result<AbilitiesListResult> {
        let abilities: Vec<AbilitySummary> = self
            .reader
            .list_abilities()
            .await?
            .into_iter()
            .map(|ability| AbilityDocument::from(ability).summary)
            .collect();
        Ok(AbilitiesListResult { abilities })
    }

    async fn get_ability(&self, params: AbilitiesGetParams) -> Result<AbilityGetResult> {
        let ability = self
            .reader
            .get_ability(params.id)
            .await?
            .ok_or_else(|| anyhow!("ability not found: {}", params.id))?;
        Ok(AbilityGetResult {
            ability: AbilityDocument::from(ability),
        })
    }

    async fn get_ability_prompt(
        &self,
        params: AbilityPromptGetParams,
    ) -> Result<AbilityPromptGetResult> {
        let ability = self
            .reader
            .get_ability(params.id)
            .await?
            .ok_or_else(|| anyhow!("ability not found: {}", params.id))?;
        Ok(AbilityPromptGetResult {
            ability: AbilityPromptDocument::from(ability),
        })
    }

    async fn create_ability(&self, params: AbilityCreateParams) -> Result<AbilityMutationResult> {
        let ability = AbilityManifest {
            id: uuid::Uuid::new_v4(),
            name: params.data.name,
            tool_name: params.data.tool_name,
            path: params.data.path,
            display_name: params.data.display_name,
            description: params.data.description,
            activation_condition: params.data.activation_condition,
            prompt_config: params.data.prompt_config,
            platform_scopes: params.data.platform_scopes.unwrap_or_default(),
            mcp_server_ids: params.data.mcp_server_ids.unwrap_or_default(),
        };
        self.writer
            .upsert_resource(&ManifestResource::Ability(ability.clone()))
            .await?;
        Ok(AbilityMutationResult {
            ability: AbilityDocument::from(ability),
        })
    }

    async fn update_ability(&self, params: AbilityUpdateParams) -> Result<AbilityMutationResult> {
        if params.data.is_empty() {
            return Err(anyhow!(
                "ability update requires at least one field in data"
            ));
        }
        let existing = self
            .reader
            .get_ability(params.id)
            .await?
            .ok_or_else(|| anyhow!("ability not found: {}", params.id))?;
        let mut ability = existing.clone();
        if let Some(display_name) = params.data.display_name {
            ability.display_name = display_name;
        }
        if let Some(tool_name) = params.data.tool_name {
            ability.tool_name = tool_name;
        }
        if let Some(description) = params.data.description {
            ability.description = description;
        }
        if let Some(activation_condition) = params.data.activation_condition {
            ability.activation_condition = activation_condition;
        }
        if let Some(platform_scopes) = params.data.platform_scopes {
            ability.platform_scopes = platform_scopes;
        }
        if let Some(mcp_server_ids) = params.data.mcp_server_ids {
            ability.mcp_server_ids = mcp_server_ids;
        }
        self.writer
            .upsert_resource(&ManifestResource::Ability(ability.clone()))
            .await?;
        Ok(AbilityMutationResult {
            ability: AbilityDocument::from(ability),
        })
    }

    async fn update_ability_prompt(
        &self,
        params: AbilityPromptUpdateParams,
    ) -> Result<AbilityPromptMutationResult> {
        let mut ability = self
            .reader
            .get_ability(params.id)
            .await?
            .ok_or_else(|| anyhow!("ability not found: {}", params.id))?;
        ability.prompt_config = params.prompt_config;
        let prompt_config = ability.prompt_config.clone();
        self.writer
            .upsert_resource(&ManifestResource::Ability(ability))
            .await?;
        Ok(AbilityPromptMutationResult { prompt_config })
    }

    async fn delete_ability(&self, params: AbilityDeleteParams) -> Result<DeleteResult> {
        self.writer
            .delete_resource(ManifestResourceKind::Ability, params.id)
            .await?;
        Ok(DeleteResult {
            deleted: true,
            id: params.id,
        })
    }
}

#[async_trait]
impl<R, W> DomainManifestBackend for LocalManifestMcpBackend<R, W>
where
    R: ManifestReader + Send + Sync,
    W: ManifestWriter + Send + Sync,
{
    async fn list_domains(&self) -> Result<DomainsListResult> {
        let domains: Vec<DomainSummary> = self
            .reader
            .list_domains()
            .await?
            .into_iter()
            .map(|domain| DomainDocument::from(domain).summary)
            .collect();
        Ok(DomainsListResult { domains })
    }

    async fn get_domain(&self, params: DomainsGetParams) -> Result<DomainGetResult> {
        let domain = self
            .reader
            .get_domain(params.id)
            .await?
            .ok_or_else(|| anyhow!("domain not found: {}", params.id))?;
        Ok(DomainGetResult {
            domain: DomainDocument::from(domain),
        })
    }

    async fn get_domain_prompt(
        &self,
        params: DomainManifestGetParams,
    ) -> Result<DomainManifestGetResult> {
        let domain = self
            .reader
            .get_domain(params.id)
            .await?
            .ok_or_else(|| anyhow!("domain not found: {}", params.id))?;
        Ok(DomainManifestGetResult {
            domain: DomainManifestDocument::from(domain),
        })
    }

    async fn create_domain(&self, params: DomainCreateParams) -> Result<DomainMutationResult> {
        let domain = DomainManifest {
            id: uuid::Uuid::new_v4(),
            name: params.data.name,
            path: params.data.path,
            display_name: params.data.display_name,
            description: params.data.description,
            command: params.data.command,
            platform_scopes: params.data.platform_scopes.unwrap_or_default(),
            ability_ids: params.data.ability_ids.unwrap_or_default(),
            mcp_server_ids: params.data.mcp_server_ids.unwrap_or_default(),
            prompt_config: params.data.prompt_config.unwrap_or_default(),
        };
        self.writer
            .upsert_resource(&ManifestResource::Domain(domain.clone()))
            .await?;
        Ok(DomainMutationResult {
            domain: DomainDocument::from(domain),
        })
    }

    async fn update_domain(&self, params: DomainUpdateParams) -> Result<DomainMutationResult> {
        let existing = self
            .reader
            .get_domain(params.id)
            .await?
            .ok_or_else(|| anyhow!("domain not found: {}", params.id))?;
        if params.data.is_empty() {
            return Err(anyhow!("domain update requires at least one field"));
        }
        let mut domain = existing.clone();
        if let Some(name) = params.data.name {
            domain.name = name;
        }
        if let Some(display_name) = params.data.display_name {
            domain.display_name = display_name;
        }
        if let Some(description) = params.data.description {
            domain.description = description;
        }
        if let Some(command) = params.data.command {
            domain.command = command;
        }
        if let Some(platform_scopes) = params.data.platform_scopes {
            domain.platform_scopes = platform_scopes;
        }
        if let Some(ability_ids) = params.data.ability_ids {
            domain.ability_ids = ability_ids;
        }
        if let Some(mcp_server_ids) = params.data.mcp_server_ids {
            domain.mcp_server_ids = mcp_server_ids;
        }
        self.writer
            .upsert_resource(&ManifestResource::Domain(domain.clone()))
            .await?;
        Ok(DomainMutationResult {
            domain: DomainDocument::from(domain),
        })
    }

    async fn update_domain_prompt(
        &self,
        params: DomainManifestUpdateParams,
    ) -> Result<DomainManifestMutationResult> {
        let mut domain = self
            .reader
            .get_domain(params.id)
            .await?
            .ok_or_else(|| anyhow!("domain not found: {}", params.id))?;
        domain.prompt_config = params.prompt_config;
        let prompt_config = domain.prompt_config.clone();
        self.writer
            .upsert_resource(&ManifestResource::Domain(domain))
            .await?;
        Ok(DomainManifestMutationResult { prompt_config })
    }

    async fn delete_domain(&self, params: DomainDeleteParams) -> Result<DeleteResult> {
        self.writer
            .delete_resource(ManifestResourceKind::Domain, params.id)
            .await?;
        Ok(DeleteResult {
            deleted: true,
            id: params.id,
        })
    }
}

#[async_trait]
impl<R, W> ProjectManifestBackend for LocalManifestMcpBackend<R, W>
where
    R: ManifestReader + Send + Sync,
    W: ManifestWriter + Send + Sync,
{
    async fn list_projects(&self) -> Result<ProjectsListResult> {
        let projects: Vec<ProjectSummary> = self
            .reader
            .list_projects()
            .await?
            .into_iter()
            .map(|project| ProjectDocument::from(project).summary)
            .collect();
        Ok(ProjectsListResult { projects })
    }

    async fn get_project(&self, params: ProjectsGetParams) -> Result<ProjectGetResult> {
        let project = self
            .reader
            .get_project(params.id)
            .await?
            .ok_or_else(|| anyhow!("project not found: {}", params.id))?;
        Ok(ProjectGetResult {
            project: ProjectDocument::from(project),
        })
    }

    async fn create_project(&self, params: ProjectCreateParams) -> Result<ProjectMutationResult> {
        let mut settings = serde_json::json!({});
        if let Some(repo_url) = params.data.repo_url
            && let Some(obj) = settings.as_object_mut()
        {
            obj.insert("repo_url".into(), serde_json::json!(repo_url));
        }
        let project = ProjectManifest {
            id: uuid::Uuid::new_v4(),
            name: params.data.name,
            slug: String::new(),
            description: params.data.description,
            settings,
        };
        self.writer
            .upsert_resource(&ManifestResource::Project(project.clone()))
            .await?;
        Ok(ProjectMutationResult {
            project: ProjectDocument::from(project),
        })
    }

    async fn update_project(&self, params: ProjectUpdateParams) -> Result<ProjectMutationResult> {
        let existing = self
            .reader
            .get_project(params.id)
            .await?
            .ok_or_else(|| anyhow!("project not found: {}", params.id))?;
        let mut project = existing.clone();
        if let Some(name) = params.data.name {
            project.name = name;
        }
        if let Some(description) = params.data.description {
            project.description = description;
        }
        if let Some(repo_url) = params.data.repo_url {
            match repo_url {
                Some(url) => {
                    if let Some(obj) = project.settings.as_object_mut() {
                        obj.insert("repo_url".into(), serde_json::json!(url));
                    } else {
                        project.settings = serde_json::json!({ "repo_url": url });
                    }
                }
                None => {
                    if let Some(obj) = project.settings.as_object_mut() {
                        obj.remove("repo_url");
                    }
                }
            }
        }
        self.writer
            .upsert_resource(&ManifestResource::Project(project.clone()))
            .await?;
        Ok(ProjectMutationResult {
            project: ProjectDocument::from(project),
        })
    }

    async fn delete_project(&self, params: ProjectDeleteParams) -> Result<DeleteResult> {
        self.writer
            .delete_resource(ManifestResourceKind::Project, params.id)
            .await?;
        Ok(DeleteResult {
            deleted: true,
            id: params.id,
        })
    }

    async fn list_project_documents(
        &self,
        _params: serde_json::Value,
    ) -> Result<serde_json::Value> {
        Err(anyhow!(
            "project documents are not available in the local manifest backend"
        ))
    }

    async fn read_project_document_manifest(
        &self,
        _params: serde_json::Value,
    ) -> Result<serde_json::Value> {
        Err(anyhow!(
            "project document manifests are not available in the local manifest backend"
        ))
    }

    async fn read_project_document(&self, _params: serde_json::Value) -> Result<serde_json::Value> {
        Err(anyhow!(
            "project document content is not available in the local manifest backend"
        ))
    }

    async fn search_project_documents(
        &self,
        _params: serde_json::Value,
    ) -> Result<serde_json::Value> {
        Err(anyhow!(
            "project document search is not available in the local manifest backend"
        ))
    }

    async fn search_project_document_paths(
        &self,
        _params: serde_json::Value,
    ) -> Result<serde_json::Value> {
        Err(anyhow!(
            "project document path search is not available in the local manifest backend"
        ))
    }

    async fn list_project_document_tree(
        &self,
        _params: serde_json::Value,
    ) -> Result<serde_json::Value> {
        Err(anyhow!(
            "project document tree is not available in the local manifest backend"
        ))
    }

    async fn list_project_document_neighbors(
        &self,
        _params: serde_json::Value,
    ) -> Result<serde_json::Value> {
        Err(anyhow!(
            "project document neighbors are not available in the local manifest backend"
        ))
    }

    async fn create_project_document(
        &self,
        _params: ProjectDocumentCreateParams,
    ) -> Result<ProjectDocumentMutationResult> {
        bail!("project document tools require the platform backend")
    }

    async fn update_project_document_content(
        &self,
        _params: ProjectDocumentContentUpdateParams,
    ) -> Result<ProjectDocumentContentMutationResult> {
        bail!("project document tools require the platform backend")
    }

    async fn delete_project_document(
        &self,
        params: ProjectDocumentDeleteParams,
    ) -> Result<DeleteResult> {
        Ok(DeleteResult {
            deleted: false,
            id: params.document_id,
        })
    }
}

#[async_trait]
impl<R, W> RoutineManifestBackend for LocalManifestMcpBackend<R, W>
where
    R: ManifestReader + Send + Sync,
    W: ManifestWriter + Send + Sync,
{
    async fn list_routines(&self) -> Result<RoutinesListResult> {
        Ok(RoutinesListResult {
            routines: self
                .reader
                .list_routines()
                .await?
                .into_iter()
                .map(|routine| RoutineDocument::from(routine).summary)
                .collect(),
        })
    }

    async fn get_routine(&self, params: RoutinesGetParams) -> Result<RoutineGetResult> {
        let routine = self
            .reader
            .get_routine(params.id)
            .await?
            .ok_or_else(|| anyhow!("routine not found: {}", params.id))?;
        Ok(RoutineGetResult {
            routine: RoutineDocument::from(routine),
        })
    }

    async fn create_routine(&self, params: RoutineCreateParams) -> Result<RoutineMutationResult> {
        let routine_id = uuid::Uuid::new_v4();
        let (steps, edges, metadata) = graph_input_to_manifest_parts(
            routine_id,
            params.data.metadata.unwrap_or_default(),
            params.data.graph,
        );
        let routine = RoutineManifest {
            id: routine_id,
            name: params.data.name,
            description: params.data.description,
            trigger: params.data.trigger.unwrap_or(RoutineTrigger::Task),
            metadata,
            steps,
            edges,
        };
        self.writer
            .upsert_resource(&ManifestResource::Routine(routine.clone()))
            .await?;
        Ok(RoutineMutationResult {
            routine: RoutineDocument::from(routine),
        })
    }

    async fn update_routine(&self, params: RoutineUpdateParams) -> Result<RoutineMutationResult> {
        if params.data.is_empty() {
            return Err(anyhow!(
                "routine update requires at least one field in data"
            ));
        }
        let existing = self
            .reader
            .get_routine(params.id)
            .await?
            .ok_or_else(|| anyhow!("routine not found: {}", params.id))?;
        let mut routine = existing.clone();
        if let Some(name) = params.data.name {
            routine.name = name;
        }
        if let Some(description) = params.data.description {
            routine.description = description;
        }
        if let Some(trigger) = params.data.trigger {
            routine.trigger = trigger;
        }
        if let Some(metadata) = params.data.metadata {
            routine.metadata = metadata;
        }
        if let Some(graph) = params.data.graph {
            let (steps, edges, metadata) =
                graph_input_to_manifest_parts(routine.id, routine.metadata.clone(), Some(graph));
            routine.steps = steps;
            routine.edges = edges;
            routine.metadata = metadata;
        }
        self.writer
            .upsert_resource(&ManifestResource::Routine(routine.clone()))
            .await?;
        Ok(RoutineMutationResult {
            routine: RoutineDocument::from(routine),
        })
    }

    async fn delete_routine(&self, params: RoutineDeleteParams) -> Result<DeleteResult> {
        self.writer
            .delete_resource(ManifestResourceKind::Routine, params.id)
            .await?;
        Ok(DeleteResult {
            deleted: true,
            id: params.id,
        })
    }
}

#[async_trait]
impl<R, W> ModelManifestBackend for LocalManifestMcpBackend<R, W>
where
    R: ManifestReader + Send + Sync,
    W: ManifestWriter + Send + Sync,
{
    async fn list_models(&self) -> Result<ModelsListResult> {
        Ok(ModelsListResult {
            models: self
                .reader
                .list_models()
                .await?
                .into_iter()
                .map(|model| ModelDocument::from(model).summary)
                .collect(),
        })
    }

    async fn get_model(&self, params: ModelsGetParams) -> Result<ModelGetResult> {
        let model = self
            .reader
            .get_model(params.id)
            .await?
            .ok_or_else(|| anyhow!("model not found: {}", params.id))?;
        Ok(ModelGetResult {
            model: ModelDocument::from(model),
        })
    }

    async fn create_model(&self, params: ModelCreateParams) -> Result<ModelMutationResult> {
        let model = ModelManifest {
            id: uuid::Uuid::new_v4(),
            name: params.data.name,
            description: params.data.description,
            model: params.data.model,
            model_provider: params
                .data
                .model_provider
                .unwrap_or_else(|| "openai".into()),
            temperature: Some(params.data.temperature.unwrap_or(0.7)),
            base_url: params.data.base_url,
        };
        self.writer
            .upsert_resource(&ManifestResource::Model(model.clone()))
            .await?;
        Ok(ModelMutationResult {
            model: ModelDocument::from(model),
        })
    }

    async fn update_model(&self, params: ModelUpdateParams) -> Result<ModelMutationResult> {
        let existing = self
            .reader
            .get_model(params.id)
            .await?
            .ok_or_else(|| anyhow!("model not found: {}", params.id))?;
        let mut model = existing.clone();
        if let Some(name) = params.data.name {
            model.name = name;
        }
        if let Some(description) = params.data.description {
            model.description = description;
        }
        if let Some(model_name) = params.data.model {
            model.model = model_name;
        }
        if let Some(model_provider) = params.data.model_provider {
            model.model_provider = model_provider;
        }
        if let Some(temperature) = params.data.temperature {
            model.temperature = Some(temperature);
        }
        if let Some(base_url) = params.data.base_url {
            model.base_url = base_url;
        }
        self.writer
            .upsert_resource(&ManifestResource::Model(model.clone()))
            .await?;
        Ok(ModelMutationResult {
            model: ModelDocument::from(model),
        })
    }

    async fn delete_model(&self, params: ModelDeleteParams) -> Result<DeleteResult> {
        self.writer
            .delete_resource(ManifestResourceKind::Model, params.id)
            .await?;
        Ok(DeleteResult {
            deleted: true,
            id: params.id,
        })
    }
}

#[async_trait]
impl<R, W> CouncilManifestBackend for LocalManifestMcpBackend<R, W>
where
    R: ManifestReader + Send + Sync,
    W: ManifestWriter + Send + Sync,
{
    async fn list_councils(&self) -> Result<CouncilsListResult> {
        Ok(CouncilsListResult {
            councils: self
                .reader
                .list_councils()
                .await?
                .into_iter()
                .map(|council| CouncilDocument::from(council).summary)
                .collect(),
        })
    }

    async fn get_council(&self, params: CouncilsGetParams) -> Result<CouncilGetResult> {
        let council = self
            .reader
            .get_council(params.id)
            .await?
            .ok_or_else(|| anyhow!("council not found: {}", params.id))?;
        Ok(CouncilGetResult {
            council: CouncilDocument::from(council),
        })
    }

    async fn create_council(&self, params: CouncilCreateParams) -> Result<CouncilMutationResult> {
        let council = CouncilManifest {
            id: uuid::Uuid::new_v4(),
            name: params.data.name,
            delegation_strategy: params
                .data
                .delegation_strategy
                .unwrap_or(CouncilDelegationStrategy::Decompose),
            leader_agent_id: params.data.leader_agent_id,
            members: params
                .data
                .members
                .into_iter()
                .map(|member| CouncilMemberManifest {
                    agent_id: member.agent_id,
                    agent_name: String::new(),
                    priority: member.priority,
                })
                .collect(),
        };
        self.writer
            .upsert_resource(&ManifestResource::Council(council.clone()))
            .await?;
        Ok(CouncilMutationResult {
            council: CouncilDocument::from(council),
        })
    }

    async fn update_council(&self, params: CouncilUpdateParams) -> Result<CouncilMutationResult> {
        let existing = self
            .reader
            .get_council(params.id)
            .await?
            .ok_or_else(|| anyhow!("council not found: {}", params.id))?;
        let mut council = existing.clone();
        if let Some(name) = params.data.name {
            council.name = name;
        }
        if let Some(delegation_strategy) = params.data.delegation_strategy {
            council.delegation_strategy = delegation_strategy;
        }
        self.writer
            .upsert_resource(&ManifestResource::Council(council.clone()))
            .await?;
        Ok(CouncilMutationResult {
            council: CouncilDocument::from(council),
        })
    }

    async fn add_council_member(
        &self,
        params: CouncilAddMemberParams,
    ) -> Result<CouncilMutationResult> {
        let mut council = self
            .reader
            .get_council(params.council_id)
            .await?
            .ok_or_else(|| anyhow!("council not found: {}", params.council_id))?;
        if council
            .members
            .iter()
            .any(|member| member.agent_id == params.data.agent_id)
        {
            bail!("council member already exists: {}", params.data.agent_id);
        }
        council.members.push(CouncilMemberManifest {
            agent_id: params.data.agent_id,
            agent_name: String::new(),
            priority: params.data.priority,
        });
        self.writer
            .upsert_resource(&ManifestResource::Council(council.clone()))
            .await?;
        Ok(CouncilMutationResult {
            council: CouncilDocument::from(council),
        })
    }

    async fn update_council_member(
        &self,
        params: CouncilUpdateMemberParams,
    ) -> Result<CouncilMutationResult> {
        if params.data.is_empty() {
            bail!("council member update requires at least one field");
        }
        let mut council = self
            .reader
            .get_council(params.council_id)
            .await?
            .ok_or_else(|| anyhow!("council not found: {}", params.council_id))?;
        let member = council
            .members
            .iter_mut()
            .find(|member| member.agent_id == params.agent_id)
            .ok_or_else(|| anyhow!("council member not found: {}", params.agent_id))?;
        if let Some(priority) = params.data.priority {
            member.priority = priority;
        }
        self.writer
            .upsert_resource(&ManifestResource::Council(council.clone()))
            .await?;
        Ok(CouncilMutationResult {
            council: CouncilDocument::from(council),
        })
    }

    async fn remove_council_member(
        &self,
        params: CouncilRemoveMemberParams,
    ) -> Result<CouncilMutationResult> {
        let mut council = self
            .reader
            .get_council(params.council_id)
            .await?
            .ok_or_else(|| anyhow!("council not found: {}", params.council_id))?;
        let original_len = council.members.len();
        council
            .members
            .retain(|member| member.agent_id != params.agent_id);
        if council.members.len() == original_len {
            bail!("council member not found: {}", params.agent_id);
        }
        self.writer
            .upsert_resource(&ManifestResource::Council(council.clone()))
            .await?;
        Ok(CouncilMutationResult {
            council: CouncilDocument::from(council),
        })
    }

    async fn delete_council(&self, params: CouncilDeleteParams) -> Result<DeleteResult> {
        self.writer
            .delete_resource(ManifestResourceKind::Council, params.id)
            .await?;
        Ok(DeleteResult {
            deleted: true,
            id: params.id,
        })
    }
}

#[async_trait]
impl<R, W> ContextBlockManifestBackend for LocalManifestMcpBackend<R, W>
where
    R: ManifestReader + Send + Sync,
    W: ManifestWriter + Send + Sync,
{
    async fn list_context_blocks(&self) -> Result<ContextBlocksListResult> {
        Ok(ContextBlocksListResult {
            context_blocks: self
                .reader
                .list_context_blocks()
                .await?
                .into_iter()
                .map(|context_block| ContextBlockDocument::from(context_block).summary)
                .collect(),
        })
    }

    async fn get_context_block(
        &self,
        params: ContextBlocksGetParams,
    ) -> Result<ContextBlockGetResult> {
        let context_block = self
            .reader
            .get_context_block(params.id)
            .await?
            .ok_or_else(|| anyhow!("context block not found: {}", params.id))?;
        Ok(ContextBlockGetResult {
            context_block: ContextBlockDocument::from(context_block),
        })
    }

    async fn get_context_block_content(
        &self,
        params: ContextBlockContentGetParams,
    ) -> Result<ContextBlockContentGetResult> {
        let context_block = self
            .reader
            .get_context_block(params.id)
            .await?
            .ok_or_else(|| anyhow!("context block not found: {}", params.id))?;
        Ok(ContextBlockContentGetResult {
            context_block: ContextBlockContentDocument::from(context_block),
        })
    }

    async fn create_context_block(
        &self,
        params: ContextBlockCreateParams,
    ) -> Result<ContextBlockMutationResult> {
        let context_block = ContextBlockManifest {
            id: uuid::Uuid::new_v4(),
            name: params.data.name,
            path: params.data.path,
            display_name: params.data.display_name,
            description: params.data.description,
            template: params.data.template,
        };
        self.writer
            .upsert_resource(&ManifestResource::ContextBlock(context_block.clone()))
            .await?;
        Ok(ContextBlockMutationResult {
            context_block: ContextBlockDocument::from(context_block),
        })
    }

    async fn update_context_block(
        &self,
        params: ContextBlockUpdateParams,
    ) -> Result<ContextBlockMutationResult> {
        let existing = self
            .reader
            .get_context_block(params.id)
            .await?
            .ok_or_else(|| anyhow!("context block not found: {}", params.id))?;
        let mut context_block = existing.clone();
        if let Some(name) = params.data.name {
            context_block.name = name;
        }
        if let Some(display_name) = params.data.display_name {
            context_block.display_name = display_name;
        }
        if let Some(description) = params.data.description {
            context_block.description = description;
        }
        self.writer
            .upsert_resource(&ManifestResource::ContextBlock(context_block.clone()))
            .await?;
        Ok(ContextBlockMutationResult {
            context_block: ContextBlockDocument::from(context_block),
        })
    }

    async fn update_context_block_content(
        &self,
        params: ContextBlockContentUpdateParams,
    ) -> Result<ContextBlockContentMutationResult> {
        let mut context_block = self
            .reader
            .get_context_block(params.id)
            .await?
            .ok_or_else(|| anyhow!("context block not found: {}", params.id))?;
        if let Some(template) = params.template {
            context_block.template = template;
        }
        let template = context_block.template.clone();
        self.writer
            .upsert_resource(&ManifestResource::ContextBlock(context_block))
            .await?;
        Ok(ContextBlockContentMutationResult { template })
    }

    async fn delete_context_block(&self, params: ContextBlockDeleteParams) -> Result<DeleteResult> {
        self.writer
            .delete_resource(ManifestResourceKind::ContextBlock, params.id)
            .await?;
        Ok(DeleteResult {
            deleted: true,
            id: params.id,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tempfile::tempdir;
    use uuid::Uuid;

    use nenjo::manifest::local::LocalManifestStore;
    use nenjo::manifest::{
        AbilityManifest, AgentManifest, ContextBlockManifest, CouncilManifest,
        CouncilMemberManifest, DomainManifest, Manifest, ModelManifest, ProjectManifest,
        PromptConfig, PromptTemplates, RoutineEdgeCondition, RoutineEdgeManifest, RoutineManifest,
        RoutineMetadata, RoutineStepManifest, RoutineStepType,
    };
    use nenjo::manifest::{AbilityPromptConfig, DomainPromptConfig};

    use super::*;
    use crate::manifest_mcp::ManifestMcpContract;
    use crate::{RoutineEdgeInput, RoutineGraphInput, RoutineStepInput};

    struct SampleManifest {
        manifest: Manifest,
        agent: AgentManifest,
        ability: AbilityManifest,
        domain: DomainManifest,
        project: ProjectManifest,
        routine: RoutineManifest,
        model: ModelManifest,
        council: CouncilManifest,
        context_block: ContextBlockManifest,
    }

    struct TestContext {
        backend: LocalManifestMcpBackend<LocalManifestStore, LocalManifestStore>,
        agent: AgentManifest,
        ability: AbilityManifest,
        domain: DomainManifest,
        project: ProjectManifest,
        routine: RoutineManifest,
        model: ModelManifest,
        council: CouncilManifest,
        context_block: ContextBlockManifest,
    }

    fn sample_manifest() -> SampleManifest {
        let model = ModelManifest {
            id: Uuid::new_v4(),
            name: "test-model".into(),
            description: None,
            model: "gpt-4o".into(),
            model_provider: "openai".into(),
            temperature: Some(0.3),
            base_url: None,
        };

        let alt_model = ModelManifest {
            id: Uuid::new_v4(),
            name: "reasoner".into(),
            description: Some("Reasoning model".into()),
            model: "gpt-5".into(),
            model_provider: "openai".into(),
            temperature: Some(0.2),
            base_url: Some("https://api.example.com".into()),
        };

        let agent = AgentManifest {
            id: Uuid::new_v4(),
            name: "coder".into(),
            description: Some("writes code".into()),
            prompt_config: PromptConfig {
                system_prompt: "You are a coding agent.".into(),
                developer_prompt: "Follow repo conventions.".into(),
                templates: PromptTemplates {
                    chat_task: "Respond to chat".into(),
                    task_execution: "Execute task".into(),
                    ..Default::default()
                },
                ..Default::default()
            },
            color: Some("#123456".into()),
            model_id: Some(model.id),
            domain_ids: vec![],
            platform_scopes: vec!["agents:read".into()],
            mcp_server_ids: vec![],
            ability_ids: vec![],
            prompt_locked: false,
            heartbeat: None,
        };

        let ability = AbilityManifest {
            id: Uuid::new_v4(),
            name: "review_helper".into(),
            tool_name: "review_helper".into(),
            path: "team/core".into(),
            display_name: Some("Review Helper".into()),
            description: Some("Helps review code".into()),
            activation_condition: "when reviewing".into(),
            prompt_config: AbilityPromptConfig {
                developer_prompt: "Review the proposed change".into(),
            },
            platform_scopes: vec!["projects:read".into()],
            mcp_server_ids: vec![],
        };

        let domain = DomainManifest {
            id: Uuid::new_v4(),
            name: "creator".into(),
            path: "team".into(),
            display_name: "Creator".into(),
            description: Some("Creates new resources".into()),
            command: "#creator".into(),
            platform_scopes: vec![],
            ability_ids: vec![],
            mcp_server_ids: vec![],
            prompt_config: DomainPromptConfig {
                developer_prompt_addon: Some("Creator mode".into()),
            },
        };

        let project = ProjectManifest {
            id: Uuid::new_v4(),
            name: "workspace".into(),
            slug: "workspace".into(),
            description: Some("Main working project".into()),
            settings: serde_json::json!({
                "repo_url": "https://example.com/repo.git"
            }),
        };

        let routine_id = Uuid::new_v4();
        let step_id = Uuid::new_v4();
        let routine = RoutineManifest {
            id: routine_id,
            name: "nightly-build".into(),
            description: Some("Runs the nightly build".into()),
            trigger: RoutineTrigger::Cron,
            metadata: nenjo::manifest::RoutineMetadata {
                schedule: Some("0 0 * * *".into()),
                entry_step_ids: vec![step_id],
            },
            steps: vec![RoutineStepManifest {
                id: step_id,
                routine_id,
                name: "compile".into(),
                step_type: nenjo::manifest::RoutineStepType::Agent,
                council_id: None,
                agent_id: Some(agent.id),
                config: serde_json::json!({}),
                order_index: 0,
            }],
            edges: vec![RoutineEdgeManifest {
                id: Uuid::new_v4(),
                routine_id,
                source_step_id: step_id,
                target_step_id: step_id,
                condition: nenjo::manifest::RoutineEdgeCondition::Always,
            }],
        };

        let council = CouncilManifest {
            id: Uuid::new_v4(),
            name: "triage".into(),
            delegation_strategy: CouncilDelegationStrategy::Decompose,
            leader_agent_id: agent.id,
            members: vec![CouncilMemberManifest {
                agent_id: Uuid::new_v4(),
                agent_name: "worker".into(),
                priority: 10,
            }],
        };

        let context_block = ContextBlockManifest {
            id: Uuid::new_v4(),
            name: "repo_summary".into(),
            path: "team/core".into(),
            display_name: Some("Repo Summary".into()),
            description: Some("Summarizes the current repository.".into()),
            template: "Repository: {{ repo_name }}".into(),
        };

        let manifest = Manifest {
            auth: Some(nenjo::manifest::ManifestAuth {
                user_id: Uuid::new_v4(),
                org_id: Uuid::new_v4(),
                api_key_id: Some(Uuid::new_v4()),
            }),
            models: vec![model, alt_model.clone()],
            agents: vec![agent],
            abilities: vec![ability.clone()],
            domains: vec![domain.clone()],
            projects: vec![project.clone()],
            routines: vec![routine.clone()],
            councils: vec![council.clone()],
            context_blocks: vec![context_block.clone()],
            ..Default::default()
        };

        let agent = manifest.agents[0].clone();
        let council = manifest.councils[0].clone();
        SampleManifest {
            manifest,
            agent,
            ability,
            domain,
            project,
            routine,
            model: alt_model,
            council,
            context_block,
        }
    }

    async fn backend() -> TestContext {
        let dir = tempdir().unwrap();
        let root = dir.keep();
        let store = Arc::new(LocalManifestStore::new(root));
        let SampleManifest {
            manifest,
            agent,
            ability,
            domain,
            project,
            routine,
            model,
            council,
            context_block,
        } = sample_manifest();
        store.replace_manifest(&manifest).await.unwrap();
        TestContext {
            backend: LocalManifestMcpBackend::new(store.clone(), store),
            agent,
            ability,
            domain,
            project,
            routine,
            model,
            council,
            context_block,
        }
    }

    #[tokio::test]
    async fn get_agent_is_prompt_free() {
        let TestContext { backend, agent, .. } = backend().await;

        let list = backend.list_agents().await.unwrap();
        assert_eq!(list.agents.len(), 1);
        assert_eq!(list.agents[0].id, agent.id);
        let list_value = serde_json::to_value(&list).unwrap();
        assert!(list_value["agents"][0].get("domains").is_none());
        assert!(list_value["agents"][0].get("abilities").is_none());
        assert!(list_value["agents"][0].get("platform_scopes").is_none());
        assert!(list_value["agents"][0].get("prompt_config").is_none());

        let result = backend
            .get_agent(AgentsGetParams { id: agent.id })
            .await
            .unwrap();
        let value = serde_json::to_value(result).unwrap();

        assert_eq!(value["agent"]["name"], agent.name);
        assert!(value["agent"].get("prompt_config").is_none());
    }

    #[tokio::test]
    async fn get_agent_includes_heartbeat_state() {
        let dir = tempdir().unwrap();
        let root = dir.keep();
        let store = Arc::new(LocalManifestStore::new(root));
        let SampleManifest {
            mut manifest,
            agent,
            ..
        } = sample_manifest();
        manifest.agents[0].heartbeat = Some(nenjo::manifest::AgentHeartbeatManifest {
            id: Uuid::new_v4(),
            agent_id: agent.id,
            interval: "5m".into(),
            is_active: true,
            last_run_at: None,
            next_run_at: None,
            metadata: serde_json::Value::Null,
        });
        store.replace_manifest(&manifest).await.unwrap();
        let backend = LocalManifestMcpBackend::new(store.clone(), store);

        let result = backend
            .get_agent(AgentsGetParams { id: agent.id })
            .await
            .unwrap();

        assert_eq!(
            result
                .agent
                .heartbeat
                .as_ref()
                .map(|heartbeat| heartbeat.interval.as_str()),
            Some("5m")
        );
    }

    #[tokio::test]
    async fn get_agent_prompt_returns_prompt_config() {
        let TestContext { backend, agent, .. } = backend().await;

        let result = backend
            .get_agent_prompt(AgentPromptGetParams { id: agent.id })
            .await
            .unwrap();

        assert_eq!(result.agent.agent.summary.name, agent.name);
        assert_eq!(
            result.agent.prompt_config.system_prompt,
            "You are a coding agent."
        );
    }

    #[tokio::test]
    async fn update_agent_merges_partial_patch() {
        let TestContext { backend, agent, .. } = backend().await;

        let result = backend
            .update_agent(AgentUpdateParams {
                id: agent.id,
                data: crate::AgentUpdateDocument {
                    name: Some("reviewer".into()),
                    description: None,
                    color: None,
                    model_id: None,
                    platform_scopes: None,
                },
            })
            .await
            .unwrap();

        assert_eq!(result.agent.summary.name, "reviewer");
        assert_eq!(result.agent.summary.description, Some("writes code".into()));
        assert_eq!(result.agent.summary.color, Some("#123456".into()));
        assert_eq!(result.agent.platform_scopes, vec!["agents:read"]);
    }

    #[tokio::test]
    async fn update_agent_can_clear_nullable_fields() {
        let TestContext { backend, agent, .. } = backend().await;

        let result = backend
            .update_agent(AgentUpdateParams {
                id: agent.id,
                data: crate::AgentUpdateDocument {
                    name: None,
                    description: Some(None),
                    color: Some(None),
                    model_id: Some(None),
                    platform_scopes: None,
                },
            })
            .await
            .unwrap();

        assert_eq!(result.agent.summary.name, "coder");
        assert_eq!(result.agent.summary.description, None);
        assert_eq!(result.agent.summary.color, None);
        assert_eq!(result.agent.summary.model_id, None);
    }

    #[tokio::test]
    async fn update_agent_prompt_merges_nested_patch() {
        let TestContext { backend, agent, .. } = backend().await;

        let result = backend
            .update_agent_prompt(AgentPromptUpdateParams {
                id: agent.id,
                prompt_config: Some(serde_json::json!({
                    "developer_prompt": "Prefer minimal diffs.",
                    "templates": {
                        "chat": "New chat template"
                    }
                })),
            })
            .await
            .unwrap();

        assert_eq!(
            result.prompt_config.system_prompt,
            "You are a coding agent."
        );
        assert_eq!(
            result.prompt_config.developer_prompt,
            "Prefer minimal diffs."
        );
        assert_eq!(
            result.prompt_config.templates.chat_task,
            "New chat template"
        );
        assert_eq!(
            result.prompt_config.templates.task_execution,
            "Execute task"
        );
    }

    #[tokio::test]
    async fn update_agent_prompt_rejects_locked_agent() {
        let dir = tempdir().unwrap();
        let root = dir.keep();
        let store = Arc::new(LocalManifestStore::new(root));
        let SampleManifest {
            mut manifest,
            agent,
            ..
        } = sample_manifest();
        manifest.agents[0].prompt_locked = true;
        store.replace_manifest(&manifest).await.unwrap();
        let backend = LocalManifestMcpBackend::new(store.clone(), store);

        let error = backend
            .update_agent_prompt(AgentPromptUpdateParams {
                id: agent.id,
                prompt_config: Some(serde_json::json!({
                    "developer_prompt": "This should fail."
                })),
            })
            .await
            .unwrap_err();

        assert!(error.to_string().contains("prompt is locked"));
    }

    #[tokio::test]
    async fn contract_dispatch_accepts_patch_style_updates() {
        let TestContext { backend, agent, .. } = backend().await;

        let result = ManifestMcpContract::dispatch(
            &backend,
            "update_agent",
            serde_json::json!({
                "id": agent.id,
                "name": "planner"
            }),
        )
        .await
        .unwrap();

        assert_eq!(result["agent"]["name"], serde_json::json!("planner"));

        let prompt_result = ManifestMcpContract::dispatch(
            &backend,
            "update_agent_prompt",
            serde_json::json!({
                "id": agent.id,
                "prompt_config": {
                    "templates": {
                        "chat": "Planner chat"
                    }
                }
            }),
        )
        .await
        .unwrap();

        assert_eq!(
            prompt_result["prompt_config"]["templates"]["chat"],
            serde_json::json!("Planner chat")
        );
        assert_eq!(
            prompt_result["prompt_config"]["system_prompt"],
            serde_json::json!("You are a coding agent.")
        );
    }

    #[tokio::test]
    async fn contract_dispatch_supports_create_agent() {
        let TestContext { backend, .. } = backend().await;

        let result = ManifestMcpContract::dispatch(
            &backend,
            "create_agent",
            serde_json::json!({
                "name": "writer",
                "description": "Writes manifests.",
                "platform_scopes": ["agents:write"]
            }),
        )
        .await
        .unwrap();

        assert_eq!(result["agent"]["name"], serde_json::json!("writer"));
        assert_eq!(
            result["agent"]["platform_scopes"],
            serde_json::json!(["agents:write"])
        );
    }

    #[tokio::test]
    async fn list_abilities_and_get_use_local_manifest() {
        let TestContext {
            backend, ability, ..
        } = backend().await;

        let list = backend.list_abilities().await.unwrap();
        assert_eq!(list.abilities.len(), 1);
        assert_eq!(list.abilities[0].id, ability.id);

        let get = backend
            .get_ability(AbilitiesGetParams { id: ability.id })
            .await
            .unwrap();
        assert_eq!(get.ability.summary.name, "review_helper");
        assert_eq!(get.ability.summary.path, "team/core");
    }

    #[tokio::test]
    async fn get_ability_prompt_returns_prompt_content() {
        let TestContext {
            backend, ability, ..
        } = backend().await;

        let get = backend
            .get_ability_prompt(AbilityPromptGetParams { id: ability.id })
            .await
            .unwrap();
        assert_eq!(get.ability.ability.summary.name, "review_helper");
        assert_eq!(
            get.ability.prompt_config.developer_prompt,
            "Review the proposed change"
        );
    }

    #[tokio::test]
    async fn update_ability_merges_partial_patch() {
        let TestContext {
            backend, ability, ..
        } = backend().await;

        let result = backend
            .update_ability(AbilityUpdateParams {
                id: ability.id,
                data: crate::AbilityUpdateDocument {
                    tool_name: None,
                    display_name: Some(Some("Reviewer".into())),
                    description: None,
                    activation_condition: None,
                    platform_scopes: None,
                    mcp_server_ids: None,
                },
            })
            .await
            .unwrap();

        assert_eq!(result.ability.summary.display_name, Some("Reviewer".into()));
        assert_eq!(
            result.ability.summary.description,
            Some("Helps review code".into())
        );
        assert_eq!(result.ability.platform_scopes, vec!["projects:read"]);
    }

    #[tokio::test]
    async fn update_ability_prompt_replaces_prompt() {
        let TestContext {
            backend, ability, ..
        } = backend().await;

        let result = backend
            .update_ability_prompt(AbilityPromptUpdateParams {
                id: ability.id,
                prompt_config: AbilityPromptConfig {
                    developer_prompt: "New review prompt".into(),
                },
            })
            .await
            .unwrap();

        assert_eq!(result.prompt_config.developer_prompt, "New review prompt");
    }

    #[tokio::test]
    async fn list_domains_and_get_use_local_manifest() {
        let TestContext {
            backend, domain, ..
        } = backend().await;

        let list = backend.list_domains().await.unwrap();
        assert_eq!(list.domains.len(), 1);
        assert_eq!(list.domains[0].id, domain.id);

        let get = backend
            .get_domain(DomainsGetParams { id: domain.id })
            .await
            .unwrap();
        assert_eq!(get.domain.summary.name, "creator");
        assert_eq!(get.domain.command, "#creator");
    }

    #[tokio::test]
    async fn get_domain_manifest_returns_manifest_content() {
        let TestContext {
            backend, domain, ..
        } = backend().await;

        let get = backend
            .get_domain_prompt(DomainManifestGetParams { id: domain.id })
            .await
            .unwrap();
        assert_eq!(get.domain.domain.summary.name, "creator");
        assert_eq!(
            get.domain.prompt_config.developer_prompt_addon,
            Some("Creator mode".to_string())
        );
    }

    #[tokio::test]
    async fn update_domain_merges_partial_patch() {
        let TestContext {
            backend, domain, ..
        } = backend().await;

        let result = backend
            .update_domain(DomainUpdateParams {
                id: domain.id,
                data: crate::DomainUpdateDocument {
                    name: None,
                    display_name: Some("Builder".into()),
                    description: Some(None),
                    command: None,
                    platform_scopes: None,
                    ability_ids: None,
                    mcp_server_ids: None,
                },
            })
            .await
            .unwrap();

        assert_eq!(result.domain.summary.name, "creator");
        assert_eq!(result.domain.summary.display_name, "Builder");
        assert_eq!(result.domain.summary.description, None);
    }

    #[tokio::test]
    async fn update_domain_manifest_replaces_manifest() {
        let TestContext {
            backend, domain, ..
        } = backend().await;

        let result = backend
            .update_domain_prompt(DomainManifestUpdateParams {
                id: domain.id,
                prompt_config: DomainPromptConfig {
                    developer_prompt_addon: Some("Builder mode".into()),
                },
            })
            .await
            .unwrap();

        assert_eq!(
            result.prompt_config.developer_prompt_addon,
            Some("Builder mode".to_string())
        );
    }

    #[tokio::test]
    async fn contract_dispatch_supports_ability_and_domain_patch_ops() {
        let TestContext {
            backend,
            ability,
            domain,
            ..
        } = backend().await;

        let ability_result = ManifestMcpContract::dispatch(
            &backend,
            "update_ability",
            serde_json::json!({
                "id": ability.id,
                "description": "Improved review helper"
            }),
        )
        .await
        .unwrap();
        assert_eq!(
            ability_result["ability"]["description"],
            serde_json::json!("Improved review helper")
        );

        let ability_prompt_result = ManifestMcpContract::dispatch(
            &backend,
            "update_ability_prompt",
            serde_json::json!({
                "id": ability.id,
                "prompt_config": {
                    "developer_prompt": "Upgraded prompt"
                }
            }),
        )
        .await
        .unwrap();
        assert_eq!(
            ability_prompt_result["prompt_config"]["developer_prompt"],
            serde_json::json!("Upgraded prompt")
        );

        let domain_result = ManifestMcpContract::dispatch(
            &backend,
            "update_domain",
            serde_json::json!({
                "id": domain.id,
                "description": "Updated creator domain"
            }),
        )
        .await
        .unwrap();
        assert_eq!(
            domain_result["domain"]["description"],
            serde_json::json!("Updated creator domain")
        );

        let domain_manifest_result = ManifestMcpContract::dispatch(
            &backend,
            "update_domain_prompt",
            serde_json::json!({
                "id": domain.id,
                "prompt_config": {
                    "developer_prompt_addon": "Build mode"
                }
            }),
        )
        .await
        .unwrap();
        assert_eq!(
            domain_manifest_result["prompt_config"]["developer_prompt_addon"],
            serde_json::json!("Build mode")
        );
    }

    #[tokio::test]
    async fn list_projects_and_get_use_local_manifest() {
        let TestContext {
            backend, project, ..
        } = backend().await;

        let list = backend.list_projects().await.unwrap();
        assert_eq!(list.projects.len(), 1);
        assert_eq!(list.projects[0].id, project.id);
        let list_value = serde_json::to_value(&list).unwrap();
        assert!(list_value["projects"][0].get("settings").is_none());

        let get = backend
            .get_project(ProjectsGetParams { id: project.id })
            .await
            .unwrap();
        assert_eq!(get.project.summary.name, "workspace");
        assert_eq!(get.project.summary.slug, "workspace");
    }

    #[tokio::test]
    async fn update_project_merges_partial_patch() {
        let TestContext {
            backend, project, ..
        } = backend().await;

        let result = backend
            .update_project(ProjectUpdateParams {
                id: project.id,
                data: crate::ProjectUpdateDocument {
                    name: Some("workspace-v2".into()),
                    description: None,
                    repo_url: None,
                },
            })
            .await
            .unwrap();

        assert_eq!(result.project.summary.name, "workspace-v2");
        assert_eq!(
            result.project.summary.description,
            Some("Main working project".into())
        );
        assert_eq!(
            result.project.settings["repo_url"],
            serde_json::json!("https://example.com/repo.git")
        );
    }

    #[tokio::test]
    async fn update_project_can_clear_description() {
        let TestContext {
            backend, project, ..
        } = backend().await;

        let result = backend
            .update_project(ProjectUpdateParams {
                id: project.id,
                data: crate::ProjectUpdateDocument {
                    name: None,
                    description: Some(None),
                    repo_url: None,
                },
            })
            .await
            .unwrap();

        assert_eq!(result.project.summary.name, "workspace");
        assert_eq!(result.project.summary.description, None);
    }

    #[tokio::test]
    async fn contract_dispatch_supports_project_patch_ops() {
        let TestContext {
            backend, project, ..
        } = backend().await;

        let result = ManifestMcpContract::dispatch(
            &backend,
            "update_project",
            serde_json::json!({
                "id": project.id,
                "repo_url": "https://example.com/next.git"
            }),
        )
        .await
        .unwrap();

        assert_eq!(
            result["project"]["settings"]["repo_url"],
            serde_json::json!("https://example.com/next.git")
        );
    }

    #[tokio::test]
    async fn list_routines_and_get_use_local_manifest() {
        let TestContext {
            backend, routine, ..
        } = backend().await;

        let list = backend.list_routines().await.unwrap();
        assert_eq!(list.routines.len(), 1);
        assert_eq!(list.routines[0].id, routine.id);
        let list_value = serde_json::to_value(&list).unwrap();
        assert!(list_value["routines"][0].get("metadata").is_none());
        assert!(list_value["routines"][0].get("steps").is_none());
        assert!(list_value["routines"][0].get("edges").is_none());

        let get = backend
            .get_routine(RoutinesGetParams { id: routine.id })
            .await
            .unwrap();
        assert_eq!(get.routine.summary.name, "nightly-build");
        assert_eq!(get.routine.summary.trigger, RoutineTrigger::Cron);
        assert_eq!(get.routine.steps.len(), 1);
    }

    #[tokio::test]
    async fn update_routine_merges_partial_patch() {
        let TestContext {
            backend, routine, ..
        } = backend().await;

        let result = backend
            .update_routine(RoutineUpdateParams {
                id: routine.id,
                data: crate::RoutineUpdateDocument {
                    name: Some("nightly-release".into()),
                    description: None,
                    trigger: None,
                    metadata: None,
                    graph: None,
                },
            })
            .await
            .unwrap();

        assert_eq!(result.routine.summary.name, "nightly-release");
        assert_eq!(
            result.routine.summary.description,
            Some("Runs the nightly build".into())
        );
        assert_eq!(result.routine.summary.trigger, RoutineTrigger::Cron);
        assert_eq!(
            result.routine.metadata.schedule.as_deref(),
            Some("0 0 * * *")
        );
        assert_eq!(
            result.routine.metadata.entry_step_ids,
            vec![routine.steps[0].id]
        );
    }

    #[tokio::test]
    async fn update_routine_can_clear_description() {
        let TestContext {
            backend, routine, ..
        } = backend().await;

        let result = backend
            .update_routine(RoutineUpdateParams {
                id: routine.id,
                data: crate::RoutineUpdateDocument {
                    name: None,
                    description: Some(None),
                    trigger: None,
                    metadata: None,
                    graph: None,
                },
            })
            .await
            .unwrap();

        assert_eq!(result.routine.summary.name, "nightly-build");
        assert_eq!(result.routine.summary.description, None);
    }

    #[tokio::test]
    async fn contract_dispatch_supports_routine_patch_ops() {
        let TestContext {
            backend, routine, ..
        } = backend().await;

        let result = ManifestMcpContract::dispatch(
            &backend,
            "update_routine",
            serde_json::json!({
                "id": routine.id,
                "metadata": {
                    "schedule": "0 6 * * *",
                    "entry_step_ids": [routine.steps[0].id]
                }
            }),
        )
        .await
        .unwrap();

        assert_eq!(
            result["routine"]["metadata"],
            serde_json::json!({
                "schedule": "0 6 * * *",
                "entry_step_ids": [routine.steps[0].id]
            })
        );
    }

    #[tokio::test]
    async fn create_routine_and_update_accept_graph_payloads() {
        let TestContext { backend, .. } = backend().await;
        let step_id = Uuid::new_v4();
        let terminal_id = Uuid::new_v4();

        let created = backend
            .create_routine(RoutineCreateParams {
                data: crate::RoutineCreateDocument {
                    name: "pipeline".into(),
                    description: Some("Build workflow".into()),
                    trigger: Some(RoutineTrigger::Task),
                    metadata: Some(RoutineMetadata {
                        schedule: None,
                        entry_step_ids: vec![],
                    }),
                    graph: Some(RoutineGraphInput {
                        entry_step_ids: vec![step_id.to_string()],
                        steps: vec![
                            RoutineStepInput {
                                step_id: step_id.to_string(),
                                name: "build".into(),
                                step_type: RoutineStepType::Agent,
                                council_id: None,
                                agent_id: None,
                                config: serde_json::json!({}),
                                order_index: 0,
                            },
                            RoutineStepInput {
                                step_id: terminal_id.to_string(),
                                name: "done".into(),
                                step_type: RoutineStepType::Terminal,
                                council_id: None,
                                agent_id: None,
                                config: serde_json::json!({}),
                                order_index: 1,
                            },
                        ],
                        edges: vec![RoutineEdgeInput {
                            source_step_id: step_id.to_string(),
                            target_step_id: terminal_id.to_string(),
                            condition: RoutineEdgeCondition::Always,
                        }],
                    }),
                },
            })
            .await
            .unwrap();

        assert_eq!(created.routine.steps.len(), 2);
        assert_eq!(created.routine.edges.len(), 1);

        let updated = backend
            .update_routine(RoutineUpdateParams {
                id: created.routine.summary.id,
                data: crate::RoutineUpdateDocument {
                    name: None,
                    description: None,
                    trigger: None,
                    metadata: None,
                    graph: Some(RoutineGraphInput {
                        entry_step_ids: vec![step_id.to_string()],
                        steps: vec![RoutineStepInput {
                            step_id: step_id.to_string(),
                            name: "build".into(),
                            step_type: RoutineStepType::Agent,
                            council_id: None,
                            agent_id: None,
                            config: serde_json::json!({ "revised": true }),
                            order_index: 0,
                        }],
                        edges: vec![],
                    }),
                },
            })
            .await
            .unwrap();

        assert_eq!(updated.routine.steps.len(), 1);
        assert!(updated.routine.edges.is_empty());
        assert_eq!(updated.routine.steps[0].config["revised"], true);
    }

    #[tokio::test]
    async fn list_models_and_get_use_local_manifest() {
        let TestContext { backend, model, .. } = backend().await;

        let list = backend.list_models().await.unwrap();
        assert_eq!(list.models.len(), 2);
        assert!(list.models.iter().any(|m| m.id == model.id));
        let reasoner = serde_json::to_value(&list).unwrap()["models"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["id"] == serde_json::json!(model.id))
            .cloned()
            .unwrap();
        assert!(reasoner.get("temperature").is_none());
        assert!(reasoner.get("tags").is_none());
        assert!(reasoner.get("base_url").is_none());

        let get = backend
            .get_model(ModelsGetParams { id: model.id })
            .await
            .unwrap();
        assert_eq!(get.model.summary.name, "reasoner");
        assert_eq!(get.model.summary.model, "gpt-5");
    }

    #[tokio::test]
    async fn update_model_merges_partial_patch() {
        let TestContext { backend, model, .. } = backend().await;

        let result = backend
            .update_model(ModelUpdateParams {
                id: model.id,
                data: crate::ModelUpdateDocument {
                    name: Some("reasoner-v2".into()),
                    description: None,
                    model: None,
                    model_provider: None,
                    temperature: Some(0.4),
                    base_url: None,
                },
            })
            .await
            .unwrap();

        assert_eq!(result.model.summary.name, "reasoner-v2");
        assert_eq!(
            result.model.summary.description,
            Some("Reasoning model".into())
        );
        assert_eq!(result.model.temperature, Some(0.4));
        assert_eq!(
            result.model.base_url,
            Some("https://api.example.com".into())
        );
    }

    #[tokio::test]
    async fn update_model_can_clear_nullable_fields() {
        let TestContext { backend, model, .. } = backend().await;

        let result = backend
            .update_model(ModelUpdateParams {
                id: model.id,
                data: crate::ModelUpdateDocument {
                    name: None,
                    description: Some(None),
                    model: None,
                    model_provider: None,
                    temperature: None,
                    base_url: Some(None),
                },
            })
            .await
            .unwrap();

        assert_eq!(result.model.summary.name, "reasoner");
        assert_eq!(result.model.summary.description, None);
        assert_eq!(result.model.base_url, None);
    }

    #[tokio::test]
    async fn contract_dispatch_supports_model_patch_ops() {
        let TestContext { backend, model, .. } = backend().await;

        let result = ManifestMcpContract::dispatch(
            &backend,
            "update_model",
            serde_json::json!({
                "id": model.id,
                "temperature": 0.42
            }),
        )
        .await
        .unwrap();

        assert_eq!(result["model"]["temperature"], serde_json::json!(0.42));
    }

    #[tokio::test]
    async fn list_councils_and_get_use_local_manifest() {
        let TestContext {
            backend, council, ..
        } = backend().await;

        let list = backend.list_councils().await.unwrap();
        assert_eq!(list.councils.len(), 1);
        assert_eq!(list.councils[0].id, council.id);
        let list_value = serde_json::to_value(&list).unwrap();
        assert!(list_value["councils"][0].get("members").is_none());

        let get = backend
            .get_council(CouncilsGetParams { id: council.id })
            .await
            .unwrap();
        assert_eq!(get.council.summary.name, "triage");
        assert_eq!(get.council.members[0].agent_name, "worker");
    }

    #[tokio::test]
    async fn update_council_merges_partial_patch() {
        let TestContext {
            backend, council, ..
        } = backend().await;

        let result = backend
            .update_council(CouncilUpdateParams {
                id: council.id,
                data: crate::CouncilUpdateDocument {
                    name: Some("dispatch".into()),
                    description: None,
                    delegation_strategy: None,
                    config: None,
                },
            })
            .await
            .unwrap();

        assert_eq!(result.council.summary.name, "dispatch");
        assert_eq!(
            result.council.summary.delegation_strategy,
            CouncilDelegationStrategy::Decompose
        );
        assert_eq!(result.council.members.len(), 1);
    }

    #[tokio::test]
    async fn contract_dispatch_supports_council_patch_ops() {
        let TestContext {
            backend, council, ..
        } = backend().await;

        let result = ManifestMcpContract::dispatch(
            &backend,
            "update_council",
            serde_json::json!({
                "id": council.id,
                "delegation_strategy": "broadcast"
            }),
        )
        .await
        .unwrap();

        assert_eq!(
            result["council"]["delegation_strategy"],
            serde_json::json!("broadcast")
        );
    }

    #[tokio::test]
    async fn contract_dispatch_supports_council_member_ops() {
        let TestContext {
            backend, council, ..
        } = backend().await;
        let member_agent_id = council.members[0].agent_id;
        let new_agent_id = Uuid::new_v4();

        let add_result = ManifestMcpContract::dispatch(
            &backend,
            "add_council_member",
            serde_json::json!({
                "council_id": council.id,
                "agent_id": new_agent_id,
                "priority": 5,
                "config": {}
            }),
        )
        .await
        .unwrap();
        assert_eq!(
            add_result["council"]["members"].as_array().unwrap().len(),
            2
        );

        let update_result = ManifestMcpContract::dispatch(
            &backend,
            "update_council_member",
            serde_json::json!({
                "council_id": council.id,
                "agent_id": member_agent_id,
                "priority": 42
            }),
        )
        .await
        .unwrap();
        assert_eq!(update_result["council"]["members"][0]["priority"], 42);

        let remove_result = ManifestMcpContract::dispatch(
            &backend,
            "remove_council_member",
            serde_json::json!({
                "council_id": council.id,
                "agent_id": new_agent_id
            }),
        )
        .await
        .unwrap();
        assert_eq!(
            remove_result["council"]["members"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn list_context_blocks_and_get_are_content_free() {
        let TestContext {
            backend,
            context_block,
            ..
        } = backend().await;

        let list = backend.list_context_blocks().await.unwrap();
        assert_eq!(list.context_blocks.len(), 1);
        assert_eq!(list.context_blocks[0].id, context_block.id);
        assert!(
            serde_json::to_value(&list).unwrap()["context_blocks"][0]
                .get("template")
                .is_none()
        );

        let get = backend
            .get_context_block(ContextBlocksGetParams {
                id: context_block.id,
            })
            .await
            .unwrap();
        assert_eq!(get.context_block.summary.name, "repo_summary");
        assert!(
            serde_json::to_value(&get).unwrap()["context_block"]
                .get("template")
                .is_none()
        );
    }

    #[tokio::test]
    async fn get_context_block_content_returns_template() {
        let TestContext {
            backend,
            context_block,
            ..
        } = backend().await;

        let result = backend
            .get_context_block_content(ContextBlockContentGetParams {
                id: context_block.id,
            })
            .await
            .unwrap();

        assert_eq!(result.context_block.template, "Repository: {{ repo_name }}");
    }

    #[tokio::test]
    async fn update_context_block_merges_partial_patch() {
        let TestContext {
            backend,
            context_block,
            ..
        } = backend().await;

        let result = backend
            .update_context_block(ContextBlockUpdateParams {
                id: context_block.id,
                data: crate::ContextBlockUpdateDocument {
                    name: None,
                    display_name: Some(Some("Repository Overview".into())),
                    description: None,
                    template: None,
                },
            })
            .await
            .unwrap();

        assert_eq!(
            result.context_block.summary.display_name,
            Some("Repository Overview".into())
        );
        assert_eq!(
            result.context_block.summary.description,
            Some("Summarizes the current repository.".into())
        );
    }

    #[tokio::test]
    async fn update_context_block_content_updates_template_only() {
        let TestContext {
            backend,
            context_block,
            ..
        } = backend().await;

        let result = backend
            .update_context_block_content(ContextBlockContentUpdateParams {
                id: context_block.id,
                template: Some("Repository: {{ repo_slug }}".into()),
            })
            .await
            .unwrap();

        assert_eq!(result.template, "Repository: {{ repo_slug }}");

        let fetched = backend
            .get_context_block_content(ContextBlockContentGetParams {
                id: context_block.id,
            })
            .await
            .unwrap();
        assert_eq!(
            fetched.context_block.template,
            "Repository: {{ repo_slug }}"
        );
    }

    #[tokio::test]
    async fn contract_dispatch_supports_context_block_content_ops() {
        let TestContext {
            backend,
            context_block,
            ..
        } = backend().await;

        let result = ManifestMcpContract::dispatch(
            &backend,
            "update_context_block_content",
            serde_json::json!({
                "id": context_block.id,
                "template": "Repository: {{ project_name }}"
            }),
        )
        .await
        .unwrap();

        assert_eq!(
            result["template"],
            serde_json::json!("Repository: {{ project_name }}")
        );
    }
}
