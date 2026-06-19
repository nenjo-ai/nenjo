//! In-process manifest MCP backend backed by a local manifest reader and writer.

use std::sync::Arc;

use anyhow::{Result, anyhow, bail};
use async_trait::async_trait;
use nenjo::{
    ManifestReader, ManifestResource, ManifestResourceKind, ManifestWriter, Slug,
    manifest::{
        AbilityManifest, AgentManifest, CommandManifest, ContextBlockManifest,
        CouncilDelegationStrategy, CouncilManifest, CouncilMemberManifest, DomainManifest,
        HasManifestSlug, ModelManifest, ProjectManifest, PromptConfig, RoutineEdgeManifest,
        RoutineManifest, RoutineMetadata, RoutineStepManifest, RoutineTrigger,
    },
};

use crate::manifest_mcp::{
    AbilitiesGetParams, AbilitiesListResult, AbilityConfigureParams, AbilityConfigureResult,
    AbilityDocument, AbilityGetResult, AbilityManifestBackend, AbilitySummary,
    AgentConfigureParams, AgentConfigureResult, AgentDocument, AgentGetResult,
    AgentManifestBackend, AgentSummary, AgentsGetParams, AgentsListResult, CommandConfigureParams,
    CommandConfigureResult, CommandGetResult, CommandManifestBackend, CommandSummary,
    CommandsGetParams, CommandsListResult, ContextBlockConfigureParams,
    ContextBlockConfigureResult, ContextBlockDocument, ContextBlockGetResult,
    ContextBlockManifestBackend, ContextBlocksGetParams, ContextBlocksListResult,
    CouncilAddMemberParams, CouncilDeleteParams, CouncilDocument, CouncilGetResult,
    CouncilManifestBackend, CouncilMutationResult, CouncilRemoveMemberParams,
    CouncilUpdateMemberParams, CouncilUpdateParams, CouncilsGetParams, CouncilsListResult,
    DeleteResult, DomainConfigureParams, DomainConfigureResult, DomainDocument, DomainGetResult,
    DomainManifestBackend, DomainSummary, DomainsGetParams, DomainsListResult,
    KnowledgeDocCreateParams, KnowledgeDocDeleteParams, KnowledgeDocMutationResult,
    KnowledgeDocUpdateParams, KnowledgePackCreateParams, KnowledgePackMutationResult,
    KnowledgePackUpdateParams, LibraryManifestBackend, ModelDeleteParams, ModelDocument,
    ModelGetResult, ModelManifestBackend, ModelMutationResult, ModelUpdateParams, ModelsGetParams,
    ModelsListResult, ProjectDeleteParams, ProjectDocument, ProjectGetResult,
    ProjectManifestBackend, ProjectMutationResult, ProjectSummary, ProjectUpdateParams,
    ProjectsGetParams, ProjectsListResult, RoutineConfigureParams, RoutineConfigureResult,
    RoutineDeleteParams, RoutineDocument, RoutineGetResult, RoutineGraphInput,
    RoutineManifestBackend, RoutinesGetParams, RoutinesListResult,
};
use crate::prompt_merge::merge_prompt_config;
use crate::{CouncilCreateParams, ModelCreateParams, ProjectCreateParams};

fn graph_input_to_manifest_parts(
    routine: Slug,
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

    metadata.entry_steps = graph.entry_steps.clone();

    let steps = graph
        .steps
        .into_iter()
        .map(|step| RoutineStepManifest {
            slug: step.slug,
            routine: routine.clone(),
            name: step.name,
            step_type: step.step_type,
            council: step.council,
            agent: step.agent,
            config: step.config,
            order_index: step.order_index,
        })
        .collect();

    let edges = graph
        .edges
        .into_iter()
        .map(|edge| RoutineEdgeManifest {
            routine: routine.clone(),
            source_step: edge.source_step,
            target_step: edge.target_step,
            condition: edge.condition,
            metadata: edge.metadata,
        })
        .collect();

    (steps, edges, metadata)
}

fn command_matches_ref(command: &CommandManifest, command_ref: &str) -> bool {
    command.name == command_ref
        || command.command == command_ref
        || command.command.trim_start_matches('/') == command_ref
}

async fn local_routine_by_slug<R>(reader: &R, routine: &Slug) -> Result<RoutineManifest>
where
    R: ManifestReader + Send + Sync,
{
    reader
        .list_routines()
        .await?
        .into_iter()
        .find(|item| Slug::derive(&item.name) == *routine)
        .ok_or_else(|| anyhow!("routine not found: {routine}"))
}

async fn local_council_by_slug<R>(reader: &R, council: &Slug) -> Result<CouncilManifest>
where
    R: ManifestReader + Send + Sync,
{
    reader
        .list_councils()
        .await?
        .into_iter()
        .find(|item| Slug::derive(&item.name) == *council)
        .ok_or_else(|| anyhow!("council not found: {council}"))
}

async fn local_agent_by_slug<R>(reader: &R, agent: &Slug) -> Result<AgentManifest>
where
    R: ManifestReader + Send + Sync,
{
    reader
        .list_agents()
        .await?
        .into_iter()
        .find(|item| Slug::derive(&item.name) == *agent)
        .ok_or_else(|| anyhow!("agent not found: {agent}"))
}

async fn local_model_by_slug<R>(reader: &R, model: &Slug) -> Result<ModelManifest>
where
    R: ManifestReader + Send + Sync,
{
    reader
        .list_models()
        .await?
        .into_iter()
        .find(|item| item.manifest_slug() == *model || Slug::derive(&item.name) == *model)
        .ok_or_else(|| anyhow!("model not found: {model}"))
}

async fn local_context_block_by_slug<R>(
    reader: &R,
    context_block: &Slug,
) -> Result<ContextBlockManifest>
where
    R: ManifestReader + Send + Sync,
{
    reader
        .list_context_blocks()
        .await?
        .into_iter()
        .find(|item| item.slug() == *context_block)
        .ok_or_else(|| anyhow!("context block not found: {context_block}"))
}

async fn local_domain_by_slug<R>(reader: &R, domain: &Slug) -> Result<DomainManifest>
where
    R: ManifestReader + Send + Sync,
{
    reader
        .list_domains()
        .await?
        .into_iter()
        .find(|item| item.slug() == *domain)
        .ok_or_else(|| anyhow!("domain not found: {domain}"))
}

async fn local_project_by_slug<R>(reader: &R, project: &Slug) -> Result<ProjectManifest>
where
    R: ManifestReader + Send + Sync,
{
    reader
        .list_projects()
        .await?
        .into_iter()
        .find(|item| item.slug == *project)
        .ok_or_else(|| anyhow!("project not found: {project}"))
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

impl<R, W> LocalManifestMcpBackend<R, W>
where
    R: ManifestReader + Send + Sync,
    W: ManifestWriter + Send + Sync,
{
    async fn resolve_ability(&self, ability_ref: &Slug) -> Result<AbilityManifest> {
        self.reader
            .list_abilities()
            .await?
            .into_iter()
            .find(|ability| Slug::derive(&ability.name) == *ability_ref)
            .ok_or_else(|| anyhow!("ability not found: {}", ability_ref))
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
        let agent = local_agent_by_slug(self.reader.as_ref(), &params.agent).await?;
        Ok(AgentGetResult {
            agent: AgentDocument::from(agent),
        })
    }

    async fn configure_agent(&self, params: AgentConfigureParams) -> Result<AgentConfigureResult> {
        let mut agent = match params.data.agent.as_ref() {
            Some(agent) => local_agent_by_slug(self.reader.as_ref(), agent).await?,
            None => {
                let name = params
                    .data
                    .metadata
                    .as_ref()
                    .and_then(|metadata| metadata.name.as_ref())
                    .ok_or_else(|| anyhow!("metadata.name is required when creating an agent"))?
                    .clone();
                AgentManifest {
                    slug: Slug::derive(&name),
                    name,
                    description: None,
                    prompt_config: PromptConfig::default(),
                    color: None,
                    model: None,
                    domains: Vec::new(),
                    platform_scopes: Vec::new(),
                    mcp_servers: Vec::new(),
                    script_tools: Vec::new(),
                    media: Vec::new(),
                    abilities: Vec::new(),
                    prompt_locked: false,
                    heartbeat: None,
                }
            }
        };

        if let Some(metadata) = params.data.metadata {
            if let Some(name) = metadata.name {
                agent.slug = Slug::derive(&name);
                agent.name = name;
            }
            if let Some(description) = metadata.description {
                agent.description = description;
            }
            if let Some(color) = metadata.color {
                agent.color = color;
            }
            if let Some(model) = metadata.model {
                agent.model = model;
            }
        }

        if let Some(prompt_patch) = params.data.prompt_config {
            if agent.prompt_locked {
                return Err(anyhow!("agent prompt is locked: {}", agent.slug));
            }
            agent.prompt_config = merge_prompt_config(&agent.prompt_config, prompt_patch)?;
        }

        if let Some(assignments) = params.data.assignments {
            if let Some(abilities) = assignments.abilities {
                agent.abilities = abilities;
            }
            if let Some(domains) = assignments.domains {
                agent.domains = domains;
            }
            if let Some(mcp_servers) = assignments.mcp_servers {
                agent.mcp_servers = mcp_servers;
            }
        }

        self.writer
            .upsert_resource(&ManifestResource::Agent(agent.clone()))
            .await?;

        Ok(AgentConfigureResult {
            agent: AgentDocument::from(agent),
            warnings: Vec::new(),
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
        let ability = self.resolve_ability(&params.ability).await?;
        Ok(AbilityGetResult {
            ability: AbilityDocument::from(ability),
        })
    }

    async fn configure_ability(
        &self,
        params: AbilityConfigureParams,
    ) -> Result<AbilityConfigureResult> {
        let mut ability = match params.data.ability.as_ref() {
            Some(ability) => self.resolve_ability(ability).await?,
            None => {
                let name = params
                    .data
                    .metadata
                    .as_ref()
                    .and_then(|metadata| metadata.name.as_ref())
                    .ok_or_else(|| anyhow!("metadata.name is required when creating an ability"))?
                    .clone();
                let prompt_config =
                    params.data.prompt_config.clone().ok_or_else(|| {
                        anyhow!("prompt_config is required when creating an ability")
                    })?;
                AbilityManifest {
                    name,
                    path: None,
                    description: None,
                    activation_condition: String::new(),
                    prompt_config,
                    platform_scopes: Vec::new(),
                    mcp_servers: Vec::new(),
                    script_tools: Vec::new(),
                    media: Vec::new(),
                    source_type: "native".to_string(),
                    read_only: false,
                    metadata: serde_json::json!({}),
                }
            }
        };

        if let Some(metadata) = params.data.metadata {
            if let Some(name) = metadata.name {
                ability.name = name;
            }
            if let Some(path) = metadata.path {
                ability.path = if path.is_empty() { None } else { Some(path) };
            }
            if let Some(description) = metadata.description {
                ability.description = description;
            }
            if let Some(activation_condition) = metadata.activation_condition {
                ability.activation_condition = activation_condition;
            }
        }

        if let Some(prompt_config) = params.data.prompt_config {
            ability.prompt_config = prompt_config;
        }

        if let Some(assignments) = params.data.assignments {
            if let Some(mcp_servers) = assignments.mcp_servers {
                ability.mcp_servers = mcp_servers;
            }
            if let Some(script_tools) = assignments.script_tools {
                ability.script_tools = script_tools;
            }
        }

        self.writer
            .upsert_resource(&ManifestResource::Ability(ability.clone()))
            .await?;

        Ok(AbilityConfigureResult {
            ability: AbilityDocument::from(ability),
            warnings: Vec::new(),
        })
    }
}

#[async_trait]
impl<R, W> CommandManifestBackend for LocalManifestMcpBackend<R, W>
where
    R: ManifestReader + Send + Sync,
    W: ManifestWriter + Send + Sync,
{
    async fn list_commands(&self) -> Result<CommandsListResult> {
        let commands = self
            .reader
            .load_manifest()
            .await?
            .commands
            .into_iter()
            .map(CommandSummary::from)
            .collect();
        Ok(CommandsListResult { commands })
    }

    async fn get_command(&self, params: CommandsGetParams) -> Result<CommandGetResult> {
        let command_ref = params.command;
        let command = self
            .reader
            .load_manifest()
            .await?
            .commands
            .into_iter()
            .find(|command| command_matches_ref(command, &command_ref))
            .ok_or_else(|| anyhow!("command not found in local manifest: {command_ref}"))?;

        Ok(CommandGetResult { command })
    }

    async fn configure_command(
        &self,
        params: CommandConfigureParams,
    ) -> Result<CommandConfigureResult> {
        let mut command = match params.data.command_ref.as_deref() {
            Some(command_ref) => {
                let manifest = self.reader.load_manifest().await?;
                let existing = manifest
                    .commands
                    .into_iter()
                    .find(|command| command_matches_ref(command, command_ref))
                    .ok_or_else(|| anyhow!("command not found in local manifest: {command_ref}"))?;
                if existing.read_only || existing.source_type != "native" {
                    return Err(anyhow!("package-managed commands cannot be edited locally"));
                }
                existing
            }
            None => {
                let metadata = params
                    .data
                    .metadata
                    .as_ref()
                    .ok_or_else(|| anyhow!("metadata is required when creating a command"))?;
                let name = metadata
                    .name
                    .clone()
                    .ok_or_else(|| anyhow!("metadata.name is required when creating a command"))?;
                let slash_command = metadata.command.clone().ok_or_else(|| {
                    anyhow!("metadata.command is required when creating a command")
                })?;
                let content = params
                    .data
                    .content
                    .clone()
                    .ok_or_else(|| anyhow!("content is required when creating a command"))?;
                CommandManifest {
                    name,
                    path: metadata.path.clone().unwrap_or_default(),
                    command: slash_command,
                    display_name: None,
                    description: metadata.description.clone().flatten(),
                    entry_path: "command.md".to_string(),
                    content,
                    root_path: String::new(),
                    root_dir: Default::default(),
                    plugin_root_path: None,
                    plugin_root_dir: None,
                    hooks: Vec::new(),
                    source_type: "native".to_string(),
                    read_only: false,
                    metadata: serde_json::json!({}),
                }
            }
        };

        if let Some(metadata) = params.data.metadata {
            if let Some(name) = metadata.name {
                command.name = name;
            }
            if let Some(path) = metadata.path {
                command.path = path;
            }
            if let Some(slash_command) = metadata.command {
                command.command = slash_command;
            }
            if let Some(description) = metadata.description {
                command.description = description;
            }
        }
        if let Some(content) = params.data.content {
            command.content = content;
        }

        self.writer
            .upsert_resource(&ManifestResource::Command(command.clone()))
            .await?;

        Ok(CommandConfigureResult {
            command,
            warnings: Vec::new(),
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
        let domain = local_domain_by_slug(self.reader.as_ref(), &params.domain).await?;
        Ok(DomainGetResult {
            domain: DomainDocument::from(domain),
        })
    }

    async fn configure_domain(
        &self,
        params: DomainConfigureParams,
    ) -> Result<DomainConfigureResult> {
        let mut domain = match params.data.domain.as_ref() {
            Some(domain) => local_domain_by_slug(self.reader.as_ref(), domain).await?,
            None => {
                let metadata = params
                    .data
                    .metadata
                    .as_ref()
                    .ok_or_else(|| anyhow!("metadata is required when creating a domain"))?;
                let name = metadata
                    .name
                    .as_ref()
                    .ok_or_else(|| anyhow!("metadata.name is required when creating a domain"))?
                    .clone();
                let command = metadata
                    .command
                    .as_ref()
                    .ok_or_else(|| anyhow!("metadata.command is required when creating a domain"))?
                    .clone();
                DomainManifest {
                    name,
                    path: String::new(),
                    description: None,
                    command,
                    platform_scopes: Vec::new(),
                    abilities: Vec::new(),
                    mcp_servers: Vec::new(),
                    script_tools: Vec::new(),
                    media: Vec::new(),
                    prompt_config: params.data.prompt_config.clone().unwrap_or_default(),
                }
            }
        };

        if let Some(metadata) = params.data.metadata {
            if let Some(name) = metadata.name {
                domain.name = name;
            }
            if let Some(path) = metadata.path {
                domain.path = path;
            }
            if let Some(description) = metadata.description {
                domain.description = description;
            }
            if let Some(command) = metadata.command {
                domain.command = command;
            }
        }

        if let Some(prompt_config) = params.data.prompt_config {
            domain.prompt_config = prompt_config;
        }

        if let Some(assignments) = params.data.assignments {
            if let Some(abilities) = assignments.abilities {
                domain.abilities = abilities;
            }
            if let Some(mcp_servers) = assignments.mcp_servers {
                domain.mcp_servers = mcp_servers;
            }
            if let Some(script_tools) = assignments.script_tools {
                domain.script_tools = script_tools;
            }
        }

        self.writer
            .upsert_resource(&ManifestResource::Domain(domain.clone()))
            .await?;

        Ok(DomainConfigureResult {
            domain: DomainDocument::from(domain),
            warnings: Vec::new(),
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
        let project = local_project_by_slug(self.reader.as_ref(), &params.project).await?;
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
            name: params.data.name.clone(),
            slug: params.data.slug,
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
        let existing = local_project_by_slug(self.reader.as_ref(), &params.project).await?;
        let mut project = existing.clone();
        if let Some(name) = params.data.name {
            project.name = name;
        }
        if let Some(slug) = params.data.slug {
            project.slug = slug;
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
        let project = local_project_by_slug(self.reader.as_ref(), &params.project).await?;
        self.writer
            .delete_resource(ManifestResourceKind::Project, &project.manifest_slug())
            .await?;
        Ok(DeleteResult { deleted: true })
    }
}

#[async_trait]
impl<R, W> LibraryManifestBackend for LocalManifestMcpBackend<R, W>
where
    R: ManifestReader + Send + Sync,
    W: ManifestWriter + Send + Sync,
{
    async fn create_knowledge_pack(
        &self,
        _params: KnowledgePackCreateParams,
    ) -> Result<KnowledgePackMutationResult> {
        bail!("library knowledge pack tools require the platform backend")
    }

    async fn update_knowledge_pack(
        &self,
        _params: KnowledgePackUpdateParams,
    ) -> Result<KnowledgePackMutationResult> {
        bail!("library knowledge pack tools require the platform backend")
    }

    async fn create_knowledge_doc(
        &self,
        _params: KnowledgeDocCreateParams,
    ) -> Result<KnowledgeDocMutationResult> {
        bail!("library knowledge document tools require the platform backend")
    }

    async fn update_knowledge_doc(
        &self,
        _params: KnowledgeDocUpdateParams,
    ) -> Result<KnowledgeDocMutationResult> {
        bail!("library knowledge document tools require the platform backend")
    }

    async fn delete_knowledge_doc(
        &self,
        _params: KnowledgeDocDeleteParams,
    ) -> Result<DeleteResult> {
        bail!("library knowledge document tools require the platform backend")
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
        let routine = local_routine_by_slug(self.reader.as_ref(), &params.slug).await?;
        Ok(RoutineGetResult {
            routine: RoutineDocument::from(routine),
        })
    }

    async fn configure_routine(
        &self,
        params: RoutineConfigureParams,
    ) -> Result<RoutineConfigureResult> {
        let mut routine = if let Some(routine_slug) = params.data.routine.as_ref() {
            local_routine_by_slug(self.reader.as_ref(), routine_slug).await?
        } else {
            let name = params
                .data
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.name.clone())
                .filter(|name| !name.trim().is_empty())
                .ok_or_else(|| anyhow!("metadata.name is required when creating a routine"))?;
            RoutineManifest {
                slug: Slug::derive(&name),
                name,
                description: None,
                trigger: RoutineTrigger::Task,
                metadata: RoutineMetadata::default(),
                steps: Vec::new(),
                edges: Vec::new(),
            }
        };

        if let Some(metadata) = params.data.metadata {
            if let Some(name) = metadata.name {
                routine.slug = Slug::derive(&name);
                routine.name = name;
            }
            if let Some(description) = metadata.description {
                routine.description = description;
            }
            if let Some(trigger) = metadata.trigger {
                routine.trigger = trigger;
            }
        }
        if let Some(runtime_metadata) = params.data.runtime_metadata {
            routine.metadata = serde_json::from_value(runtime_metadata)
                .map_err(|err| anyhow!("invalid routine runtime_metadata: {err}"))?;
        }
        if let Some(graph) = params.data.graph {
            let (steps, edges, metadata) = graph_input_to_manifest_parts(
                routine.slug().clone(),
                routine.metadata,
                Some(graph),
            );
            routine.steps = steps;
            routine.edges = edges;
            routine.metadata = metadata;
        }
        self.writer
            .upsert_resource(&ManifestResource::Routine(routine.clone()))
            .await?;
        Ok(RoutineConfigureResult {
            routine: RoutineDocument::from(routine),
            warnings: Vec::new(),
        })
    }

    async fn delete_routine(&self, params: RoutineDeleteParams) -> Result<DeleteResult> {
        let routine = local_routine_by_slug(self.reader.as_ref(), &params.slug).await?;
        self.writer
            .delete_resource(ManifestResourceKind::Routine, &routine.manifest_slug())
            .await?;
        Ok(DeleteResult { deleted: true })
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
        let model = local_model_by_slug(self.reader.as_ref(), &params.model).await?;
        Ok(ModelGetResult {
            model: ModelDocument::from(model),
        })
    }

    async fn create_model(&self, params: ModelCreateParams) -> Result<ModelMutationResult> {
        let model = ModelManifest {
            name: params.data.name,
            slug: nenjo::manifest::model_manifest_slug(
                params.data.model_provider.as_deref().unwrap_or("openai"),
                &params.data.model,
            ),
            description: params.data.description,
            model: params.data.model,
            model_provider: params
                .data
                .model_provider
                .unwrap_or_else(|| "openai".into()),
            temperature: Some(params.data.temperature.unwrap_or(0.7)),
            base_url: params.data.base_url,
            native_tools: params.data.native_tools,
        };
        self.writer
            .upsert_resource(&ManifestResource::Model(model.clone()))
            .await?;
        Ok(ModelMutationResult {
            model: ModelDocument::from(model),
        })
    }

    async fn update_model(&self, params: ModelUpdateParams) -> Result<ModelMutationResult> {
        let existing = local_model_by_slug(self.reader.as_ref(), &params.model).await?;
        let mut model = existing.clone();
        if let Some(name) = params.data.name {
            model.name = name;
        }
        if let Some(description) = params.data.description {
            model.description = description;
        }
        if let Some(model_ref) = params.data.model {
            model.model = model_ref;
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
        if let Some(native_tools) = params.data.native_tools {
            model.native_tools = native_tools;
        }
        self.writer
            .upsert_resource(&ManifestResource::Model(model.clone()))
            .await?;
        Ok(ModelMutationResult {
            model: ModelDocument::from(model),
        })
    }

    async fn delete_model(&self, params: ModelDeleteParams) -> Result<DeleteResult> {
        let model = local_model_by_slug(self.reader.as_ref(), &params.model).await?;
        self.writer
            .delete_resource(ManifestResourceKind::Model, &model.manifest_slug())
            .await?;
        Ok(DeleteResult { deleted: true })
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
        let council = local_council_by_slug(self.reader.as_ref(), &params.council).await?;
        Ok(CouncilGetResult {
            council: CouncilDocument::from(council),
        })
    }

    async fn create_council(&self, params: CouncilCreateParams) -> Result<CouncilMutationResult> {
        let council = CouncilManifest {
            name: params.data.name,
            delegation_strategy: params
                .data
                .delegation_strategy
                .unwrap_or(CouncilDelegationStrategy::Decompose),
            leader_agent: params.data.leader_agent,
            members: params
                .data
                .members
                .into_iter()
                .map(|member| CouncilMemberManifest {
                    agent: member.agent,
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
        let existing = local_council_by_slug(self.reader.as_ref(), &params.council).await?;
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
        let mut council = local_council_by_slug(self.reader.as_ref(), &params.council).await?;
        if council
            .members
            .iter()
            .any(|member| member.agent == params.data.agent)
        {
            bail!("council member already exists: {}", params.data.agent);
        }
        council.members.push(CouncilMemberManifest {
            agent: params.data.agent,
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
        let mut council = local_council_by_slug(self.reader.as_ref(), &params.council).await?;
        let member = council
            .members
            .iter_mut()
            .find(|member| member.agent == params.agent)
            .ok_or_else(|| anyhow!("council member not found: {}", params.agent))?;
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
        let mut council = local_council_by_slug(self.reader.as_ref(), &params.council).await?;
        let original_len = council.members.len();
        council
            .members
            .retain(|member| member.agent != params.agent);
        if council.members.len() == original_len {
            bail!("council member not found: {}", params.agent);
        }
        self.writer
            .upsert_resource(&ManifestResource::Council(council.clone()))
            .await?;
        Ok(CouncilMutationResult {
            council: CouncilDocument::from(council),
        })
    }

    async fn delete_council(&self, params: CouncilDeleteParams) -> Result<DeleteResult> {
        let council = local_council_by_slug(self.reader.as_ref(), &params.council).await?;
        self.writer
            .delete_resource(ManifestResourceKind::Council, &council.manifest_slug())
            .await?;
        Ok(DeleteResult { deleted: true })
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
        let context_block =
            local_context_block_by_slug(self.reader.as_ref(), &params.context_block).await?;
        Ok(ContextBlockGetResult {
            context_block: ContextBlockDocument::from(context_block),
        })
    }

    async fn configure_context_block(
        &self,
        params: ContextBlockConfigureParams,
    ) -> Result<ContextBlockConfigureResult> {
        let mut context_block = match params.data.context_block.as_ref() {
            Some(context_block) => {
                local_context_block_by_slug(self.reader.as_ref(), context_block).await?
            }
            None => {
                let name = params
                    .data
                    .metadata
                    .as_ref()
                    .and_then(|metadata| metadata.name.as_ref())
                    .ok_or_else(|| {
                        anyhow!("metadata.name is required when creating a context block")
                    })?
                    .clone();
                let template =
                    params.data.template.clone().ok_or_else(|| {
                        anyhow!("template is required when creating a context block")
                    })?;
                ContextBlockManifest {
                    name,
                    path: String::new(),
                    description: None,
                    template,
                }
            }
        };

        if let Some(metadata) = params.data.metadata {
            if let Some(name) = metadata.name {
                context_block.name = name;
            }
            if let Some(path) = metadata.path {
                context_block.path = path;
            }
            if let Some(description) = metadata.description {
                context_block.description = description;
            }
        }
        if let Some(template) = params.data.template {
            context_block.template = template;
        }

        self.writer
            .upsert_resource(&ManifestResource::ContextBlock(context_block.clone()))
            .await?;
        Ok(ContextBlockConfigureResult {
            context_block: ContextBlockDocument::from(context_block),
            warnings: Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tempfile::tempdir;

    use nenjo::manifest::local::LocalManifestStore;
    use nenjo::manifest::{
        AbilityManifest, AgentManifest, ContextBlockManifest, CouncilManifest,
        CouncilMemberManifest, DomainManifest, Manifest, ModelManifest, ProjectManifest,
        PromptConfig, PromptTemplates, RoutineEdgeCondition, RoutineEdgeManifest, RoutineManifest,
        RoutineStepManifest, RoutineStepType,
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
        command: CommandManifest,
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
        command: CommandManifest,
        context_block: ContextBlockManifest,
    }

    fn sample_manifest() -> SampleManifest {
        let model = ModelManifest {
            slug: Slug::derive("test-model"),
            name: "test-model".into(),
            description: None,
            model: "gpt-4o".into(),
            model_provider: "openai".into(),
            temperature: Some(0.3),
            base_url: None,
            native_tools: vec![],
        };

        let alt_model = ModelManifest {
            slug: Slug::derive("reasoner"),
            name: "Reasoning Model".into(),
            description: Some("Reasoning model".into()),
            model: "gpt-5".into(),
            model_provider: "openai".into(),
            temperature: Some(0.2),
            base_url: Some("https://api.example.com".into()),
            native_tools: vec![],
        };

        let agent = AgentManifest {
            name: "coder".into(),
            slug: Slug::derive("coder"),
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
            model: Some(Slug::derive(&model.name)),
            domains: vec![],
            platform_scopes: vec!["agents:read".into()],
            mcp_servers: vec![],
            abilities: vec![],
            script_tools: vec![],
            media: Vec::new(),
            prompt_locked: false,
            heartbeat: None,
        };

        let ability = AbilityManifest {
            name: "review_helper".into(),
            path: Some("team/core".into()),
            description: Some("Helps review code".into()),
            activation_condition: "when reviewing".into(),
            prompt_config: AbilityPromptConfig {
                developer_prompt: "Review the proposed change".into(),
            },
            platform_scopes: vec!["projects:read".into()],
            mcp_servers: vec![],
            script_tools: Vec::new(),
            media: Vec::new(),
            source_type: "native".into(),
            read_only: false,
            metadata: serde_json::Value::Null,
        };

        let domain = DomainManifest {
            name: "creator".into(),
            path: "team".into(),
            description: Some("Creates new resources".into()),
            command: "#creator".into(),
            platform_scopes: vec![],
            abilities: vec![],
            mcp_servers: vec![],
            script_tools: vec![],
            media: vec![],
            prompt_config: DomainPromptConfig {
                developer_prompt_addon: Some("Creator mode".into()),
            },
        };

        let project = ProjectManifest {
            name: "workspace".into(),
            slug: Slug::derive("workspace"),
            description: Some("Main working project".into()),
            settings: serde_json::json!({
                "repo_url": "https://example.com/repo.git"
            }),
        };

        let routine = RoutineManifest {
            name: "nightly-build".into(),
            slug: Slug::derive("nightly-build"),
            description: Some("Runs the nightly build".into()),
            trigger: RoutineTrigger::Cron,
            metadata: nenjo::manifest::RoutineMetadata {
                schedule: Some("0 0 * * *".into()),
                entry_steps: vec![Slug::derive("compile")],
                cron_task: None,
            },
            steps: vec![RoutineStepManifest {
                slug: Slug::derive("compile"),
                routine: Slug::derive("nightly-build"),
                name: "compile".into(),
                step_type: nenjo::manifest::RoutineStepType::Agent,
                council: None,
                agent: Some(Slug::derive(&agent.name)),
                config: serde_json::json!({}),
                order_index: 0,
            }],
            edges: vec![RoutineEdgeManifest {
                routine: Slug::derive("nightly-build"),
                source_step: Slug::derive("compile"),
                target_step: Slug::derive("compile"),
                condition: nenjo::manifest::RoutineEdgeCondition::Always,
                metadata: serde_json::json!({}),
            }],
        };

        let council = CouncilManifest {
            name: "triage".into(),
            delegation_strategy: CouncilDelegationStrategy::Decompose,
            leader_agent: Slug::derive(&agent.name),
            members: vec![CouncilMemberManifest {
                agent: Slug::derive("worker"),
                priority: 10,
            }],
        };

        let command = CommandManifest {
            name: "design".into(),
            path: "build".into(),
            command: "/design".into(),
            display_name: Some("Design".into()),
            description: Some("Design resources".into()),
            entry_path: "command.md".into(),
            content: "Design command body".into(),
            root_path: String::new(),
            root_dir: Default::default(),
            plugin_root_path: None,
            plugin_root_dir: None,
            hooks: vec![Slug::derive("prepare-design")],
            source_type: "native".into(),
            read_only: false,
            metadata: serde_json::json!({ "library": "core" }),
        };

        let context_block = ContextBlockManifest {
            name: "repo_summary".into(),
            path: "team/core".into(),
            description: Some("Summarizes the current repository.".into()),
            template: "Repository: {{ repo_name }}".into(),
        };

        let manifest = Manifest {
            models: vec![model, alt_model.clone()],
            agents: vec![agent],
            abilities: vec![ability.clone()],
            domains: vec![domain.clone()],
            projects: vec![project.clone()],
            routines: vec![routine.clone()],
            councils: vec![council.clone()],
            commands: vec![command.clone()],
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
            command,
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
            command,
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
            command,
            context_block,
        }
    }

    #[tokio::test]
    async fn list_agent_is_prompt_free_and_get_agent_includes_prompt() {
        let TestContext { backend, agent, .. } = backend().await;

        let list = backend.list_agents().await.unwrap();
        assert_eq!(list.agents.len(), 1);
        assert_eq!(list.agents[0].name, agent.name);
        let list_value = serde_json::to_value(&list).unwrap();
        assert!(list_value["agents"][0].get("domains").is_none());
        assert!(list_value["agents"][0].get("abilities").is_none());
        assert!(list_value["agents"][0].get("platform_scopes").is_none());
        assert!(list_value["agents"][0].get("prompt_config").is_none());

        let result = backend
            .get_agent(AgentsGetParams {
                agent: Slug::derive(&agent.name),
            })
            .await
            .unwrap();
        let value = serde_json::to_value(result).unwrap();

        assert_eq!(value["agent"]["name"], agent.name);
        assert_eq!(
            value["agent"]["prompt_config"]["system_prompt"],
            "You are a coding agent."
        );
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
            agent: Slug::derive(&agent.name),
            interval: "5m".into(),
            is_active: true,
            instructions: None,
            last_run_at: None,
            next_run_at: None,
            metadata: serde_json::Value::Null,
        });
        store.replace_manifest(&manifest).await.unwrap();
        let backend = LocalManifestMcpBackend::new(store.clone(), store);

        let result = backend
            .get_agent(AgentsGetParams {
                agent: Slug::derive(&agent.name),
            })
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
    async fn configure_agent_merges_metadata_patch() {
        let TestContext { backend, agent, .. } = backend().await;

        let result = backend
            .configure_agent(AgentConfigureParams {
                data: crate::AgentConfigureDocument {
                    agent: Some(Slug::derive(&agent.name)),
                    metadata: Some(crate::AgentConfigureMetadata {
                        name: Some("reviewer".into()),
                        description: None,
                        color: None,
                        model: None,
                    }),
                    prompt_config: None,
                    assignments: None,
                    ..Default::default()
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
    async fn configure_agent_can_clear_nullable_fields() {
        let TestContext { backend, agent, .. } = backend().await;

        let result = backend
            .configure_agent(AgentConfigureParams {
                data: crate::AgentConfigureDocument {
                    agent: Some(Slug::derive(&agent.name)),
                    metadata: Some(crate::AgentConfigureMetadata {
                        name: None,
                        description: Some(None),
                        color: Some(None),
                        model: Some(None),
                    }),
                    prompt_config: None,
                    assignments: None,
                    ..Default::default()
                },
            })
            .await
            .unwrap();

        assert_eq!(result.agent.summary.name, "coder");
        assert_eq!(result.agent.summary.description, None);
        assert_eq!(result.agent.summary.color, None);
        assert_eq!(result.agent.summary.model, None);
    }

    #[tokio::test]
    async fn configure_agent_can_replace_mcp_server_assignments() {
        let TestContext { backend, agent, .. } = backend().await;

        let result = backend
            .configure_agent(AgentConfigureParams {
                data: crate::AgentConfigureDocument {
                    agent: Some(Slug::derive(&agent.name)),
                    metadata: None,
                    prompt_config: None,
                    assignments: Some(crate::AgentConfigureAssignments {
                        abilities: None,
                        domains: None,
                        mcp_servers: Some(vec![Slug::derive("review-server")]),
                    }),
                    ..Default::default()
                },
            })
            .await
            .unwrap();

        assert_eq!(
            result.agent.mcp_servers,
            vec![Slug::derive("review-server")]
        );
    }

    #[tokio::test]
    async fn configure_agent_updates_prompt_and_assignments() {
        let TestContext { backend, agent, .. } = backend().await;

        let result = backend
            .configure_agent(AgentConfigureParams {
                data: crate::AgentConfigureDocument {
                    agent: Some(Slug::derive(&agent.name)),
                    metadata: Some(crate::AgentConfigureMetadata {
                        name: Some("builder".into()),
                        description: Some(Some("builds agents".into())),
                        color: None,
                        model: None,
                    }),
                    prompt_config: Some(serde_json::json!({
                        "developer_prompt": "Build agent specs carefully."
                    })),
                    assignments: Some(crate::AgentConfigureAssignments {
                        abilities: Some(vec!["design_agent".into()]),
                        domains: Some(vec![Slug::derive("creator")]),
                        mcp_servers: Some(vec![Slug::derive("review-server")]),
                    }),
                    ..Default::default()
                },
            })
            .await
            .unwrap();

        assert_eq!(result.agent.summary.slug, Slug::derive("builder"));
        assert_eq!(
            result.agent.summary.description.as_deref(),
            Some("builds agents")
        );
        assert_eq!(
            result.agent.prompt_config.developer_prompt,
            "Build agent specs carefully."
        );
        assert_eq!(result.agent.abilities, vec!["design_agent"]);
        assert_eq!(result.agent.domains, vec![Slug::derive("creator")]);
        assert_eq!(
            result.agent.mcp_servers,
            vec![Slug::derive("review-server")]
        );
    }

    #[tokio::test]
    async fn configure_agent_prompt_merges_nested_patch() {
        let TestContext { backend, agent, .. } = backend().await;

        let result = backend
            .configure_agent(AgentConfigureParams {
                data: crate::AgentConfigureDocument {
                    agent: Some(Slug::derive(&agent.name)),
                    metadata: None,
                    prompt_config: Some(serde_json::json!({
                        "developer_prompt": "Prefer minimal diffs.",
                        "templates": {
                            "chat": "New chat template"
                        }
                    })),
                    assignments: None,
                    ..Default::default()
                },
            })
            .await
            .unwrap();

        let prompt_config = result.agent.prompt_config;
        assert_eq!(prompt_config.system_prompt, "You are a coding agent.");
        assert_eq!(prompt_config.developer_prompt, "Prefer minimal diffs.");
        assert_eq!(prompt_config.templates.chat_task, "New chat template");
        assert_eq!(prompt_config.templates.task_execution, "Execute task");
    }

    #[tokio::test]
    async fn configure_agent_prompt_rejects_locked_agent() {
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
            .configure_agent(AgentConfigureParams {
                data: crate::AgentConfigureDocument {
                    agent: Some(Slug::derive(&agent.name)),
                    metadata: None,
                    prompt_config: Some(serde_json::json!({
                        "developer_prompt": "This should fail."
                    })),
                    assignments: None,
                    ..Default::default()
                },
            })
            .await
            .unwrap_err();

        assert!(error.to_string().contains("prompt is locked"));
    }

    #[tokio::test]
    async fn contract_dispatch_configures_agent() {
        let TestContext { backend, agent, .. } = backend().await;

        let result = ManifestMcpContract::dispatch(
            &backend,
            "configure_agent",
            serde_json::json!({
                "agent": Slug::derive(&agent.name),
                "metadata": {
                    "name": "planner"
                },
                "prompt_config": {
                    "templates": {
                        "chat": "Planner chat"
                    }
                }
            }),
        )
        .await
        .unwrap();

        assert_eq!(result["agent"]["name"], serde_json::json!("planner"));
        assert_eq!(
            result["agent"]["prompt_config"]["templates"]["chat"],
            serde_json::json!("Planner chat")
        );
    }

    #[tokio::test]
    async fn configure_agent_ignores_platform_scopes() {
        let TestContext { backend, agent, .. } = backend().await;

        let result = ManifestMcpContract::dispatch(
            &backend,
            "configure_agent",
            serde_json::json!({
                "agent": Slug::derive(&agent.name),
                "metadata": {
                    "name": "writer"
                },
                "platform_scopes": ["agents:write"]
            }),
        )
        .await
        .unwrap();

        assert_eq!(result["agent"]["name"], serde_json::json!("writer"));
        assert_eq!(
            result["agent"]["platform_scopes"],
            serde_json::json!(["agents:read"])
        );
    }

    #[tokio::test]
    async fn configure_agent_supports_create() {
        let TestContext { backend, .. } = backend().await;

        let result = ManifestMcpContract::dispatch(
            &backend,
            "configure_agent",
            serde_json::json!({
                "metadata": {
                    "name": "writer",
                    "description": "Writes manifests."
                }
            }),
        )
        .await
        .unwrap();

        assert_eq!(result["agent"]["name"], serde_json::json!("writer"));
        assert_eq!(result["agent"]["platform_scopes"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn list_abilities_and_get_use_local_manifest() {
        let TestContext {
            backend, ability, ..
        } = backend().await;

        let list = backend.list_abilities().await.unwrap();
        assert_eq!(list.abilities.len(), 1);
        assert_eq!(list.abilities[0].name, ability.name);

        let get = backend
            .get_ability(AbilitiesGetParams {
                ability: Slug::derive(&ability.name),
            })
            .await
            .unwrap();
        assert_eq!(get.ability.summary.name, "review_helper");
        assert_eq!(get.ability.summary.path, "team/core");
    }

    #[tokio::test]
    async fn get_ability_returns_prompt_content() {
        let TestContext {
            backend, ability, ..
        } = backend().await;

        let get = backend
            .get_ability(AbilitiesGetParams {
                ability: Slug::derive(&ability.name),
            })
            .await
            .unwrap();
        assert_eq!(get.ability.summary.name, "review_helper");
        assert_eq!(
            get.ability.prompt_config.developer_prompt,
            "Review the proposed change"
        );
    }

    #[tokio::test]
    async fn configure_ability_merges_partial_patch() {
        let TestContext {
            backend, ability, ..
        } = backend().await;

        let result = backend
            .configure_ability(AbilityConfigureParams {
                data: crate::AbilityConfigureDocument {
                    ability: Some(Slug::derive(&ability.name)),
                    metadata: Some(crate::AbilityConfigureMetadata {
                        activation_condition: Some("when reviewing code".into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            })
            .await
            .unwrap();

        assert_eq!(result.ability.summary.name, "review_helper");
        assert_eq!(
            result.ability.summary.description,
            Some("Helps review code".into())
        );
        assert_eq!(result.ability.activation_condition, "when reviewing code");
        assert_eq!(result.ability.platform_scopes, vec!["projects:read"]);
    }

    #[tokio::test]
    async fn contract_dispatch_does_not_modify_ability_platform_scopes() {
        let TestContext {
            backend, ability, ..
        } = backend().await;

        let result = ManifestMcpContract::dispatch(
            &backend,
            "configure_ability",
            serde_json::json!({
                "ability": ability.name,
                "metadata": {
                    "description": "Updated"
                },
                "platform_scopes": ["projects:write"]
            }),
        )
        .await
        .unwrap();

        assert_eq!(
            result["ability"]["description"],
            serde_json::json!("Updated")
        );
        assert_eq!(
            result["ability"]["platform_scopes"],
            serde_json::json!(["projects:read"])
        );
    }

    #[tokio::test]
    async fn configure_ability_replaces_prompt() {
        let TestContext {
            backend, ability, ..
        } = backend().await;

        let result = backend
            .configure_ability(AbilityConfigureParams {
                data: crate::AbilityConfigureDocument {
                    ability: Some(Slug::derive(&ability.name)),
                    prompt_config: Some(AbilityPromptConfig {
                        developer_prompt: "New review prompt".into(),
                    }),
                    ..Default::default()
                },
            })
            .await
            .unwrap();

        assert_eq!(
            result.ability.prompt_config.developer_prompt,
            "New review prompt"
        );
    }

    #[tokio::test]
    async fn list_commands_is_content_free_and_get_command_includes_content() {
        let TestContext {
            backend, command, ..
        } = backend().await;

        let list = backend.list_commands().await.unwrap();
        assert_eq!(list.commands.len(), 1);
        assert_eq!(list.commands[0].name, command.name);
        assert_eq!(list.commands[0].path, "build");
        assert_eq!(list.commands[0].command, "/design");

        let list_value = serde_json::to_value(&list).unwrap();
        assert!(list_value["commands"][0].get("content").is_none());
        assert!(list_value["commands"][0].get("entry_path").is_none());

        let get = backend
            .get_command(CommandsGetParams {
                command: "/design".into(),
            })
            .await
            .unwrap();
        assert_eq!(get.command.name, "design");
        assert_eq!(get.command.content, "Design command body");
    }

    #[tokio::test]
    async fn contract_dispatch_lists_and_gets_commands() {
        let TestContext { backend, .. } = backend().await;

        let list = ManifestMcpContract::dispatch(&backend, "list_commands", serde_json::json!({}))
            .await
            .unwrap();
        assert_eq!(list["commands"][0]["name"], "design");
        assert!(list["commands"][0].get("content").is_none());

        let get = ManifestMcpContract::dispatch(
            &backend,
            "get_command",
            serde_json::json!({ "command": "design" }),
        )
        .await
        .unwrap();
        assert_eq!(get["command"]["command"], "/design");
        assert_eq!(get["command"]["content"], "Design command body");
    }

    #[tokio::test]
    async fn list_domains_and_get_use_local_manifest() {
        let TestContext {
            backend, domain, ..
        } = backend().await;

        let list = backend.list_domains().await.unwrap();
        assert_eq!(list.domains.len(), 1);
        assert_eq!(list.domains[0].slug, domain.slug());

        let get = backend
            .get_domain(DomainsGetParams {
                domain: domain.slug(),
            })
            .await
            .unwrap();
        assert_eq!(get.domain.summary.name, "creator");
        assert_eq!(get.domain.command, "#creator");
    }

    #[tokio::test]
    async fn get_domain_returns_prompt_content() {
        let TestContext {
            backend, domain, ..
        } = backend().await;

        let get = backend
            .get_domain(DomainsGetParams {
                domain: domain.slug(),
            })
            .await
            .unwrap();
        assert_eq!(get.domain.summary.name, "creator");
        assert_eq!(
            get.domain.prompt_config.developer_prompt_addon,
            Some("Creator mode".to_string())
        );
    }

    #[tokio::test]
    async fn configure_domain_merges_partial_patch() {
        let TestContext {
            backend, domain, ..
        } = backend().await;

        let result = backend
            .configure_domain(DomainConfigureParams {
                data: crate::DomainConfigureDocument {
                    domain: Some(domain.slug()),
                    metadata: Some(crate::DomainConfigureMetadata {
                        description: Some(None),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            })
            .await
            .unwrap();

        assert_eq!(result.domain.summary.name, "creator");
        assert_eq!(result.domain.summary.description, None);
        assert_eq!(result.domain.platform_scopes, domain.platform_scopes);
    }

    #[tokio::test]
    async fn contract_dispatch_does_not_modify_domain_platform_scopes() {
        let TestContext {
            backend, domain, ..
        } = backend().await;

        let result = ManifestMcpContract::dispatch(
            &backend,
            "configure_domain",
            serde_json::json!({
                "domain": domain.slug(),
                "metadata": {
                    "description": "Updated"
                },
                "platform_scopes": ["agents:write"]
            }),
        )
        .await
        .unwrap();

        assert_eq!(
            result["domain"]["description"],
            serde_json::json!("Updated")
        );
        assert_eq!(result["domain"]["platform_scopes"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn configure_domain_replaces_prompt() {
        let TestContext {
            backend, domain, ..
        } = backend().await;

        let result = backend
            .configure_domain(DomainConfigureParams {
                data: crate::DomainConfigureDocument {
                    domain: Some(domain.slug()),
                    prompt_config: Some(DomainPromptConfig {
                        developer_prompt_addon: Some("Builder mode".into()),
                    }),
                    ..Default::default()
                },
            })
            .await
            .unwrap();

        assert_eq!(
            result.domain.prompt_config.developer_prompt_addon,
            Some("Builder mode".to_string())
        );
    }

    #[tokio::test]
    async fn contract_dispatch_supports_ability_and_domain_configure_ops() {
        let TestContext {
            backend,
            ability,
            domain,
            ..
        } = backend().await;

        let ability_result = ManifestMcpContract::dispatch(
            &backend,
            "configure_ability",
            serde_json::json!({
                "ability": ability.name,
                "metadata": {
                    "description": "Improved review helper"
                }
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
            "configure_ability",
            serde_json::json!({
                "ability": ability.name,
                "prompt_config": {
                    "developer_prompt": "Upgraded prompt"
                }
            }),
        )
        .await
        .unwrap();
        assert_eq!(
            ability_prompt_result["ability"]["prompt_config"]["developer_prompt"],
            serde_json::json!("Upgraded prompt")
        );

        let domain_result = ManifestMcpContract::dispatch(
            &backend,
            "configure_domain",
            serde_json::json!({
                "domain": domain.slug(),
                "metadata": {
                    "description": "Updated creator domain"
                }
            }),
        )
        .await
        .unwrap();
        assert_eq!(
            domain_result["domain"]["description"],
            serde_json::json!("Updated creator domain")
        );

        let domain_prompt_result = ManifestMcpContract::dispatch(
            &backend,
            "configure_domain",
            serde_json::json!({
                "domain": domain.slug(),
                "prompt_config": {
                    "developer_prompt_addon": "Build mode"
                }
            }),
        )
        .await
        .unwrap();
        assert_eq!(
            domain_prompt_result["domain"]["prompt_config"]["developer_prompt_addon"],
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
        assert_eq!(list.projects[0].slug, project.slug);
        let list_value = serde_json::to_value(&list).unwrap();
        assert!(list_value["projects"][0].get("settings").is_none());

        let get = backend
            .get_project(ProjectsGetParams {
                project: project.slug.clone(),
            })
            .await
            .unwrap();
        assert_eq!(get.project.summary.name, "workspace");
        assert_eq!(get.project.summary.slug.as_str(), "workspace");
    }

    #[tokio::test]
    async fn update_project_merges_partial_patch() {
        let TestContext {
            backend, project, ..
        } = backend().await;

        let result = backend
            .update_project(ProjectUpdateParams {
                project: project.slug.clone(),
                data: crate::ProjectUpdateDocument {
                    name: Some("workspace-v2".into()),
                    slug: Some(Slug::derive("test-agent")),
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
                project: project.slug.clone(),
                data: crate::ProjectUpdateDocument {
                    name: None,
                    slug: Some(Slug::derive("test-agent")),
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
                "project": project.slug,
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
        assert_eq!(list.routines[0].slug, routine.slug().clone());
        let list_value = serde_json::to_value(&list).unwrap();
        assert!(list_value["routines"][0].get("id").is_none());
        assert_eq!(
            list_value["routines"][0]["slug"],
            serde_json::json!("nightly-build")
        );
        assert!(list_value["routines"][0].get("metadata").is_none());
        assert!(list_value["routines"][0].get("steps").is_none());
        assert!(list_value["routines"][0].get("edges").is_none());

        let get = backend
            .get_routine(RoutinesGetParams {
                slug: Slug::derive(&routine.name),
            })
            .await
            .unwrap();
        assert_eq!(get.routine.summary.name, "nightly-build");
        assert_eq!(get.routine.summary.trigger, RoutineTrigger::Cron);
        assert_eq!(get.routine.steps.len(), 1);
        let get_value = serde_json::to_value(&get).unwrap();
        assert!(get_value["routine"].get("id").is_none());
        assert!(get_value["routine"]["steps"][0].get("id").is_none());
        assert!(get_value["routine"]["edges"][0].get("id").is_none());
    }

    #[tokio::test]
    async fn configure_routine_merges_partial_patch() {
        let TestContext {
            backend, routine, ..
        } = backend().await;

        let result = backend
            .configure_routine(RoutineConfigureParams {
                data: crate::RoutineConfigureDocument {
                    routine: Some(Slug::derive(&routine.name)),
                    metadata: Some(crate::RoutineConfigureMetadata {
                        name: Some("nightly-release".into()),
                        description: None,
                        project_id: None,
                        trigger: None,
                        is_active: None,
                        max_retries: None,
                    }),
                    runtime_metadata: None,
                    encrypted_payload: None,
                    graph: None,
                    cron_task: None,
                    id: None,
                },
            })
            .await
            .unwrap();

        assert_eq!(result.routine.summary.name, "nightly-release");
        assert_eq!(result.routine.summary.slug, Slug::derive("nightly-release"));
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
            result.routine.metadata.entry_steps,
            vec![routine.steps[0].slug.clone()]
        );
    }

    #[tokio::test]
    async fn configure_routine_can_clear_description() {
        let TestContext {
            backend, routine, ..
        } = backend().await;

        let result = backend
            .configure_routine(RoutineConfigureParams {
                data: crate::RoutineConfigureDocument {
                    routine: Some(Slug::derive(&routine.name)),
                    metadata: Some(crate::RoutineConfigureMetadata {
                        name: None,
                        description: Some(None),
                        project_id: None,
                        trigger: None,
                        is_active: None,
                        max_retries: None,
                    }),
                    runtime_metadata: None,
                    encrypted_payload: None,
                    graph: None,
                    cron_task: None,
                    id: None,
                },
            })
            .await
            .unwrap();

        assert_eq!(result.routine.summary.name, "nightly-build");
        assert_eq!(result.routine.summary.description, None);
    }

    #[tokio::test]
    async fn contract_dispatch_supports_routine_configure() {
        let TestContext {
            backend, routine, ..
        } = backend().await;

        let result = ManifestMcpContract::dispatch(
            &backend,
            "configure_routine",
            serde_json::json!({
                "routine": Slug::derive(&routine.name),
                "runtime_metadata": {
                    "schedule": "0 6 * * *",
                    "entry_steps": [routine.steps[0].slug]
                }
            }),
        )
        .await
        .unwrap();

        assert_eq!(
            result["routine"]["metadata"],
            serde_json::json!({
                "schedule": "0 6 * * *",
                "entry_steps": [routine.steps[0].slug]
            })
        );
    }

    #[tokio::test]
    async fn configure_routine_accepts_graph_payloads() {
        let TestContext { backend, .. } = backend().await;

        let created = backend
            .configure_routine(RoutineConfigureParams {
                data: crate::RoutineConfigureDocument {
                    id: None,
                    routine: None,
                    metadata: Some(crate::RoutineConfigureMetadata {
                        name: Some("pipeline".into()),
                        description: Some(Some("Build workflow".into())),
                        project_id: None,
                        trigger: Some(RoutineTrigger::Task),
                        is_active: None,
                        max_retries: None,
                    }),
                    runtime_metadata: Some(serde_json::json!({})),
                    encrypted_payload: None,
                    cron_task: None,
                    graph: Some(RoutineGraphInput {
                        entry_steps: vec![Slug::derive("build")],
                        steps: vec![
                            RoutineStepInput {
                                id: None,
                                slug: Slug::derive("build"),
                                name: "build".into(),
                                step_type: RoutineStepType::Agent,
                                council: None,
                                agent: None,
                                config: serde_json::json!({}),
                                encrypted_payload: None,
                                order_index: 0,
                            },
                            RoutineStepInput {
                                id: None,
                                slug: Slug::derive("done"),
                                name: "done".into(),
                                step_type: RoutineStepType::Terminal,
                                council: None,
                                agent: None,
                                config: serde_json::json!({}),
                                encrypted_payload: None,
                                order_index: 1,
                            },
                        ],
                        edges: vec![RoutineEdgeInput {
                            source_step: Slug::derive("build"),
                            target_step: Slug::derive("done"),
                            condition: RoutineEdgeCondition::Always,
                            metadata: serde_json::json!({}),
                        }],
                    }),
                },
            })
            .await
            .unwrap();

        assert_eq!(created.routine.steps.len(), 2);
        assert_eq!(created.routine.edges.len(), 1);

        let updated = backend
            .configure_routine(RoutineConfigureParams {
                data: crate::RoutineConfigureDocument {
                    id: None,
                    routine: Some(created.routine.summary.slug.clone()),
                    metadata: None,
                    runtime_metadata: None,
                    encrypted_payload: None,
                    cron_task: None,
                    graph: Some(RoutineGraphInput {
                        entry_steps: vec![Slug::derive("build")],
                        steps: vec![RoutineStepInput {
                            id: None,
                            slug: Slug::derive("build"),
                            name: "build".into(),
                            step_type: RoutineStepType::Agent,
                            council: None,
                            agent: None,
                            config: serde_json::json!({ "revised": true }),
                            encrypted_payload: None,
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
        assert!(
            list.models
                .iter()
                .any(|m| m.model == model.model && m.model_provider == model.model_provider)
        );
        let reasoner = serde_json::to_value(&list).unwrap()["models"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["name"] == serde_json::json!(model.name))
            .cloned()
            .unwrap();
        assert_eq!(reasoner["slug"], serde_json::json!(model.slug));
        assert!(reasoner.get("temperature").is_none());
        assert!(reasoner.get("tags").is_none());
        assert!(reasoner.get("base_url").is_none());

        let get = backend
            .get_model(ModelsGetParams {
                model: model.slug.clone(),
            })
            .await
            .unwrap();
        assert_eq!(get.model.summary.name, model.name);
        assert_eq!(get.model.summary.model, "gpt-5");
    }

    #[tokio::test]
    async fn update_model_merges_partial_patch() {
        let TestContext { backend, model, .. } = backend().await;

        let result = backend
            .update_model(ModelUpdateParams {
                model: model.slug.clone(),
                data: crate::ModelUpdateDocument {
                    name: Some("reasoner-v2".into()),
                    description: None,
                    model: None,
                    model_provider: None,
                    temperature: Some(0.4),
                    base_url: None,
                    native_tools: None,
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
                model: model.slug.clone(),
                data: crate::ModelUpdateDocument {
                    name: None,
                    description: Some(None),
                    model: None,
                    model_provider: None,
                    temperature: None,
                    base_url: Some(None),
                    native_tools: None,
                },
            })
            .await
            .unwrap();

        assert_eq!(result.model.summary.name, "Reasoning Model");
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
                "model": Slug::derive(&model.name),
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
        assert_eq!(list.councils[0].name, council.name);
        let list_value = serde_json::to_value(&list).unwrap();
        assert!(list_value["councils"][0].get("members").is_none());

        let get = backend
            .get_council(CouncilsGetParams {
                council: Slug::derive(&council.name),
            })
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
                council: Slug::derive(&council.name),
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
                "council": Slug::derive(&council.name),
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
        let member_agent = Slug::derive("worker");
        let new_agent = Slug::derive("new-agent");

        let add_result = ManifestMcpContract::dispatch(
            &backend,
            "add_council_member",
            serde_json::json!({
                "council": Slug::derive(&council.name),
                "agent": new_agent,
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
                "council": Slug::derive(&council.name),
                "agent": member_agent,
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
                "council": Slug::derive(&council.name),
                "agent": new_agent
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
        assert_eq!(list.context_blocks[0].name, context_block.name);
        assert_eq!(list.context_blocks[0].slug, context_block.slug());
        assert_eq!(
            list.context_blocks[0].slug.as_str(),
            "team-core-repo_summary"
        );
        assert_eq!(
            list.context_blocks[0].selector,
            "{{ team.core.repo_summary }}"
        );
        assert!(
            serde_json::to_value(&list).unwrap()["context_blocks"][0]
                .get("template")
                .is_none()
        );

        let get = backend
            .get_context_block(ContextBlocksGetParams {
                context_block: context_block.slug(),
            })
            .await
            .unwrap();
        assert_eq!(get.context_block.summary.name, "repo_summary");
        assert_eq!(get.context_block.summary.slug, context_block.slug());
        assert_eq!(
            get.context_block.summary.selector,
            "{{ team.core.repo_summary }}"
        );
        assert_eq!(get.context_block.template, "Repository: {{ repo_name }}");
    }

    #[tokio::test]
    async fn get_context_block_returns_template() {
        let TestContext {
            backend,
            context_block,
            ..
        } = backend().await;

        let result = backend
            .get_context_block(ContextBlocksGetParams {
                context_block: context_block.slug(),
            })
            .await
            .unwrap();

        assert_eq!(result.context_block.template, "Repository: {{ repo_name }}");
    }

    #[tokio::test]
    async fn configure_context_block_merges_partial_patch() {
        let TestContext {
            backend,
            context_block,
            ..
        } = backend().await;

        let result = backend
            .configure_context_block(ContextBlockConfigureParams {
                data: crate::ContextBlockConfigureDocument {
                    context_block: Some(context_block.slug()),
                    metadata: Some(crate::ContextBlockConfigureMetadata {
                        description: Some(Some("Updated repository summary.".into())),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            })
            .await
            .unwrap();

        assert_eq!(
            result.context_block.summary.description,
            Some("Updated repository summary.".into())
        );
        assert_eq!(result.context_block.template, "Repository: {{ repo_name }}");
    }

    #[tokio::test]
    async fn configure_context_block_updates_template_only() {
        let TestContext {
            backend,
            context_block,
            ..
        } = backend().await;

        let result = backend
            .configure_context_block(ContextBlockConfigureParams {
                data: crate::ContextBlockConfigureDocument {
                    context_block: Some(context_block.slug()),
                    template: Some("Repository: {{ repo_slug }}".into()),
                    ..Default::default()
                },
            })
            .await
            .unwrap();

        assert_eq!(result.context_block.template, "Repository: {{ repo_slug }}");

        let fetched = backend
            .get_context_block(ContextBlocksGetParams {
                context_block: context_block.slug(),
            })
            .await
            .unwrap();
        assert_eq!(
            fetched.context_block.template,
            "Repository: {{ repo_slug }}"
        );
    }

    #[tokio::test]
    async fn contract_dispatch_supports_context_block_configure_ops() {
        let TestContext {
            backend,
            context_block,
            ..
        } = backend().await;

        let result = ManifestMcpContract::dispatch(
            &backend,
            "configure_context_block",
            serde_json::json!({
                "context_block": context_block.slug(),
                "template": "Repository: {{ project_name }}"
            }),
        )
        .await
        .unwrap();

        assert_eq!(
            result["context_block"]["template"],
            serde_json::json!("Repository: {{ project_name }}")
        );
    }
}
