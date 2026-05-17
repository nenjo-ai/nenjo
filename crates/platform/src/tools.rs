//! Tool implementations for platform manifest and REST operations.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use nenjo::manifest::store::ManifestReader;
use nenjo::{Tool, ToolCategory, ToolResult};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::json;
use uuid::Uuid;

use crate::{
    ManifestAccessPolicy, ManifestKind, ManifestMcpBackend, ManifestMcpContract,
    PlatformManifestClient, ScopeResource, SensitivePayloadEncoder,
    client::{CreateExecutionRequest, ProjectExecutionListQuery, ProjectTaskListQuery},
    rest::projects::project_rest_tools,
};

const AGENT_READ_TOOLS: &[&str] = &["list_agents", "get_agent", "get_agent_prompt"];
const AGENT_WRITE_TOOLS: &[&str] = &[
    "create_agent",
    "update_agent",
    "update_agent_prompt",
    "delete_agent",
];
const ABILITY_READ_TOOLS: &[&str] = &["list_abilities", "get_ability", "get_ability_prompt"];
const ABILITY_WRITE_TOOLS: &[&str] = &[
    "create_ability",
    "update_ability",
    "update_ability_prompt",
    "delete_ability",
];
const DOMAIN_READ_TOOLS: &[&str] = &["list_domains", "get_domain", "get_domain_prompt"];
const DOMAIN_WRITE_TOOLS: &[&str] = &[
    "create_domain",
    "update_domain",
    "update_domain_prompt",
    "delete_domain",
];
const KNOWLEDGE_READ_TOOLS: &[&str] = &[
    "list_knowledge_packs",
    "list_knowledge_docs",
    "read_knowledge_doc",
    "read_knowledge_doc_manifest",
    "search_knowledge",
    "search_knowledge_paths",
    "list_knowledge_tree",
    "list_knowledge_neighbors",
];
const PROJECT_MANIFEST_READ_TOOLS: &[&str] = &["list_projects", "get_project"];
const PROJECT_REST_READ_TOOLS: &[&str] = &[
    "list_project_tasks",
    "get_project_task",
    "list_project_execution_runs",
    "get_project_execution_run",
];
const PROJECT_MANIFEST_WRITE_TOOLS: &[&str] = &[
    "create_project",
    "update_project",
    "delete_project",
    "create_knowledge_item",
    "update_knowledge_item_content",
    "delete_knowledge_item",
];
const PROJECT_REST_WRITE_TOOLS: &[&str] = &[
    "create_project_tasks",
    "update_project_task",
    "delete_project_task",
    "start_project_execution",
    "pause_project_execution",
    "resume_project_execution",
];
const ROUTINE_READ_TOOLS: &[&str] = &["list_routines", "get_routine"];
const ROUTINE_WRITE_TOOLS: &[&str] = &["create_routine", "update_routine", "delete_routine"];
const MODEL_READ_TOOLS: &[&str] = &["list_models", "get_model"];
const MODEL_WRITE_TOOLS: &[&str] = &["create_model", "update_model", "delete_model"];
const COUNCIL_READ_TOOLS: &[&str] = &["list_councils", "get_council"];
const COUNCIL_WRITE_TOOLS: &[&str] = &[
    "create_council",
    "update_council",
    "add_council_member",
    "remove_council_member",
    "delete_council",
];
const CONTEXT_BLOCK_READ_TOOLS: &[&str] = &[
    "list_context_blocks",
    "get_context_block",
    "get_context_block_content",
];
const CONTEXT_BLOCK_WRITE_TOOLS: &[&str] = &[
    "create_context_block",
    "update_context_block",
    "update_context_block_content",
    "delete_context_block",
];

const MANIFEST_TOOL_GROUPS: &[(ScopeResource, &[&str], &[&str])] = &[
    (ScopeResource::Agents, AGENT_READ_TOOLS, AGENT_WRITE_TOOLS),
    (
        ScopeResource::Abilities,
        ABILITY_READ_TOOLS,
        ABILITY_WRITE_TOOLS,
    ),
    (
        ScopeResource::Domains,
        DOMAIN_READ_TOOLS,
        DOMAIN_WRITE_TOOLS,
    ),
    (
        ScopeResource::Routines,
        ROUTINE_READ_TOOLS,
        ROUTINE_WRITE_TOOLS,
    ),
    (ScopeResource::Models, MODEL_READ_TOOLS, MODEL_WRITE_TOOLS),
    (
        ScopeResource::Councils,
        COUNCIL_READ_TOOLS,
        COUNCIL_WRITE_TOOLS,
    ),
    (
        ScopeResource::ContextBlocks,
        CONTEXT_BLOCK_READ_TOOLS,
        CONTEXT_BLOCK_WRITE_TOOLS,
    ),
];

pub fn add_manifest_tools(
    tools: &mut Vec<Arc<dyn Tool>>,
    backend: Arc<dyn ManifestMcpBackend>,
    policy: &ManifestAccessPolicy,
) {
    let specs = manifest_tool_specs();
    add_named_manifest_tools(tools, backend.clone(), &specs, KNOWLEDGE_READ_TOOLS);
    for (resource, read_tools, write_tools) in MANIFEST_TOOL_GROUPS {
        if policy.can_read_resource(*resource) {
            add_named_manifest_tools(tools, backend.clone(), &specs, read_tools);
        }
        if policy.can_write_resource(*resource) {
            add_named_manifest_tools(tools, backend.clone(), &specs, write_tools);
        }
    }
}

pub fn add_project_rest_tools<S, E>(
    tools: &mut Vec<Arc<dyn Tool>>,
    manifest_backend: Option<Arc<dyn ManifestMcpBackend>>,
    project_backend: Option<PlatformProjectToolsBackend<S, E>>,
    policy: &ManifestAccessPolicy,
) where
    S: ManifestReader + 'static,
    E: SensitivePayloadEncoder + Clone + Send + Sync + 'static,
{
    let specs = manifest_tool_specs();
    if policy.can_read_resource(ScopeResource::Projects) {
        if let Some(backend) = manifest_backend.as_ref() {
            add_named_manifest_tools(tools, backend.clone(), &specs, PROJECT_MANIFEST_READ_TOOLS);
        }
        if let Some(backend) = project_backend.as_ref() {
            add_named_project_rest_tools(tools, backend.clone(), PROJECT_REST_READ_TOOLS);
        }
    }
    if policy.can_write_resource(ScopeResource::Projects) {
        if let Some(backend) = manifest_backend.as_ref() {
            add_named_manifest_tools(tools, backend.clone(), &specs, PROJECT_MANIFEST_WRITE_TOOLS);
        }
        if let Some(backend) = project_backend.as_ref() {
            add_named_project_rest_tools(tools, backend.clone(), PROJECT_REST_WRITE_TOOLS);
        }
    }
}

fn add_named_manifest_tools(
    tools: &mut Vec<Arc<dyn Tool>>,
    backend: Arc<dyn ManifestMcpBackend>,
    specs: &HashMap<String, nenjo::ToolSpec>,
    tool_names: &[&str],
) {
    for tool_name in tool_names {
        let Some(spec) = specs.get(*tool_name) else {
            continue;
        };
        if tools.iter().any(|existing| existing.name() == spec.name) {
            continue;
        }
        tools.push(Arc::new(ManifestContractTool::new(
            spec.clone(),
            backend.clone(),
        )));
    }
}

fn add_named_project_rest_tools<S, E>(
    tools: &mut Vec<Arc<dyn Tool>>,
    backend: PlatformProjectToolsBackend<S, E>,
    tool_names: &[&str],
) where
    S: ManifestReader + 'static,
    E: SensitivePayloadEncoder + Clone + Send + Sync + 'static,
{
    for tool_name in tool_names {
        if tools.iter().any(|existing| existing.name() == *tool_name) {
            continue;
        }
        if let Some(tool) = ProjectRestTool::from_name(tool_name, backend.clone()) {
            tools.push(Arc::new(tool));
        }
    }
}

fn manifest_tool_specs() -> HashMap<String, nenjo::ToolSpec> {
    ManifestMcpContract::tools()
        .into_iter()
        .map(|spec| (spec.name.clone(), spec))
        .collect()
}

struct ManifestContractTool {
    spec: nenjo::ToolSpec,
    backend: Arc<dyn ManifestMcpBackend>,
}

pub struct PlatformProjectToolsBackend<S, E> {
    pub client: Arc<PlatformManifestClient>,
    pub manifest_store: Arc<S>,
    pub payload_encoder: E,
    pub cached_org_id: Option<Uuid>,
}

impl<S, E> Clone for PlatformProjectToolsBackend<S, E>
where
    E: Clone,
{
    fn clone(&self) -> Self {
        Self {
            client: self.client.clone(),
            manifest_store: self.manifest_store.clone(),
            payload_encoder: self.payload_encoder.clone(),
            cached_org_id: self.cached_org_id,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct TaskContentPayload {
    description: Option<String>,
    acceptance_criteria: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct CurrentTaskState {
    description: Option<String>,
    acceptance_criteria: Option<String>,
}

impl<S, E> PlatformProjectToolsBackend<S, E>
where
    S: ManifestReader + 'static,
    E: SensitivePayloadEncoder + Clone + Send + Sync + 'static,
{
    async fn normalize_project_tool_args(
        &self,
        mut args: serde_json::Value,
        kind: ProjectRestToolKind,
    ) -> Result<serde_json::Value> {
        if !matches!(
            kind,
            ProjectRestToolKind::ListProjectTasks
                | ProjectRestToolKind::CreateProjectTasks
                | ProjectRestToolKind::ListProjectExecutionRuns
                | ProjectRestToolKind::StartProjectExecution
        ) {
            return Ok(args);
        }

        let Some(object) = args.as_object_mut() else {
            return Ok(args);
        };
        let Some((field_name, raw_project_id)) = object
            .get("project_id")
            .or_else(|| object.get("id"))
            .and_then(|value| value.as_str())
            .map(|value| {
                if object.contains_key("project_id") {
                    ("project_id", value.to_string())
                } else {
                    ("id", value.to_string())
                }
            })
        else {
            return Ok(args);
        };

        if Uuid::parse_str(&raw_project_id).is_ok() {
            return Ok(args);
        }

        let manifest = self.manifest_store.load_manifest().await?;
        let project_ids = manifest
            .projects
            .iter()
            .map(|project| project.id)
            .collect::<Vec<_>>();

        if let Some(corrected) = unique_near_uuid_match(&raw_project_id, &project_ids) {
            object.insert(field_name.to_string(), json!(corrected));
        }

        Ok(args)
    }

    async fn org_id(&self) -> Result<Uuid> {
        if let Some(org_id) = self.cached_org_id {
            return Ok(org_id);
        }

        self.client
            .current_org_id()
            .await
            .context("failed to derive org_id from authenticated API key")
    }

    async fn encode_task_payload(
        &self,
        task_id: Uuid,
        payload: &TaskContentPayload,
    ) -> Result<serde_json::Value> {
        let org_id = self.org_id().await?;
        self.payload_encoder
            .encode_payload(
                org_id,
                task_id,
                ManifestKind::Task
                    .encrypted_object_type()
                    .expect("task content object type"),
                &serde_json::to_value(payload).context("failed to encode task content payload")?,
            )
            .await?
            .context("task payload encoder did not produce encrypted payload")
    }

    async fn maybe_encode_task_payload(
        &self,
        task_id: Uuid,
        payload: &TaskContentPayload,
    ) -> Result<Option<serde_json::Value>> {
        if payload.description.is_none() && payload.acceptance_criteria.is_none() {
            return Ok(None);
        }
        self.encode_task_payload(task_id, payload).await.map(Some)
    }

    async fn create_task_body(
        &self,
        project_id: Uuid,
        args: &CreateProjectTaskItemArgs,
    ) -> Result<serde_json::Value> {
        let task_id = Uuid::new_v4();
        let payload = TaskContentPayload {
            description: args.description.clone(),
            acceptance_criteria: args.acceptance_criteria.clone(),
        };
        let encrypted_payload = self.maybe_encode_task_payload(task_id, &payload).await?;

        Ok(json!({
            "id": task_id,
            "project_id": project_id,
            "title": args.title,
            "status": args.status,
            "priority": args.priority,
            "type": args.task_type,
            "metadata": args.metadata,
            "tags": args.tags,
            "required_tags": args.required_tags,
            "complexity": args.complexity,
            "order_index": args.order_index,
            "assigned_agent_id": args.assigned_agent_id,
            "routine_id": args.routine_id,
            "encrypted_payload": encrypted_payload,
        }))
    }

    async fn create_tasks_body(&self, args: &CreateProjectTasksArgs) -> Result<serde_json::Value> {
        let mut tasks = Vec::with_capacity(args.tasks.len());
        for task in &args.tasks {
            let body = self.create_task_body(args.project_id, task).await?;
            tasks.push(body);
        }
        Ok(json!({ "tasks": tasks }))
    }

    async fn update_task_body(&self, args: &UpdateProjectTaskArgs) -> Result<serde_json::Value> {
        let mut body = serde_json::Map::new();

        if let Some(status) = args.status.as_ref() {
            body.insert("status".into(), json!(status));
        }
        if let Some(priority) = args.priority.as_ref() {
            body.insert("priority".into(), json!(priority));
        }
        if let Some(task_type) = args.task_type.as_ref() {
            body.insert("type".into(), json!(task_type));
        }
        if let Some(metadata) = args.metadata.as_ref() {
            body.insert("metadata".into(), metadata.clone());
        }
        if let Some(tags) = args.tags.as_ref() {
            body.insert("tags".into(), json!(tags));
        }
        if let Some(required_tags) = args.required_tags.as_ref() {
            body.insert("required_tags".into(), json!(required_tags));
        }
        if let Some(complexity) = args.complexity {
            body.insert("complexity".into(), json!(complexity));
        }
        if let Some(order_index) = args.order_index {
            body.insert("order_index".into(), json!(order_index));
        }
        if let Some(assigned_agent_id) = args.assigned_agent_id {
            body.insert("assigned_agent_id".into(), json!(assigned_agent_id));
        }
        if let Some(routine_id) = args.routine_id {
            body.insert("routine_id".into(), json!(routine_id));
        }

        if let Some(title) = args.title.as_ref() {
            body.insert("title".into(), json!(title));
        }

        let needs_encrypted_payload =
            args.description.is_some() || args.acceptance_criteria.is_some();

        if needs_encrypted_payload {
            let current_state = if args.description.is_some() && args.acceptance_criteria.is_some()
            {
                CurrentTaskState::default()
            } else {
                let current = self.client.get_project_task(args.task_id).await?;
                let mut current_state: CurrentTaskState =
                    serde_json::from_value(current.clone())
                        .context("failed to decode current task state")?;
                if let Some(encrypted_payload) = current
                    .get("encrypted_payload")
                    .cloned()
                    .map(serde_json::from_value::<nenjo_events::EncryptedPayload>)
                    .transpose()
                    .context("failed to parse current task encrypted payload")?
                {
                    let decoded_payload = self
                        .payload_encoder
                        .decode_payload(&serde_json::to_value(encrypted_payload)?)
                        .await?
                        .context("task payload encoder did not decode encrypted payload")?;
                    current_state = serde_json::from_value(decoded_payload)
                        .context("failed to decode current task encrypted state")?;
                }
                current_state
            };
            let payload = TaskContentPayload {
                description: args.description.clone().or(current_state.description),
                acceptance_criteria: args
                    .acceptance_criteria
                    .clone()
                    .or(current_state.acceptance_criteria),
            };
            body.insert(
                "encrypted_payload".into(),
                self.encode_task_payload(args.task_id, &payload).await?,
            );
        }

        Ok(serde_json::Value::Object(body))
    }
}

#[derive(Debug, Clone, Copy)]
enum ProjectRestToolKind {
    ListProjectTasks,
    GetProjectTask,
    CreateProjectTasks,
    UpdateProjectTask,
    DeleteProjectTask,
    ListProjectExecutionRuns,
    GetProjectExecutionRun,
    StartProjectExecution,
    PauseProjectExecution,
    ResumeProjectExecution,
}

impl ProjectRestToolKind {
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "list_project_tasks" => Some(Self::ListProjectTasks),
            "get_project_task" => Some(Self::GetProjectTask),
            "create_project_tasks" => Some(Self::CreateProjectTasks),
            "update_project_task" => Some(Self::UpdateProjectTask),
            "delete_project_task" => Some(Self::DeleteProjectTask),
            "list_project_execution_runs" => Some(Self::ListProjectExecutionRuns),
            "get_project_execution_run" => Some(Self::GetProjectExecutionRun),
            "start_project_execution" => Some(Self::StartProjectExecution),
            "pause_project_execution" => Some(Self::PauseProjectExecution),
            "resume_project_execution" => Some(Self::ResumeProjectExecution),
            _ => None,
        }
    }

    fn tool_name(&self) -> &'static str {
        match self {
            Self::ListProjectTasks => "list_project_tasks",
            Self::GetProjectTask => "get_project_task",
            Self::CreateProjectTasks => "create_project_tasks",
            Self::UpdateProjectTask => "update_project_task",
            Self::DeleteProjectTask => "delete_project_task",
            Self::ListProjectExecutionRuns => "list_project_execution_runs",
            Self::GetProjectExecutionRun => "get_project_execution_run",
            Self::StartProjectExecution => "start_project_execution",
            Self::PauseProjectExecution => "pause_project_execution",
            Self::ResumeProjectExecution => "resume_project_execution",
        }
    }
}

struct ProjectRestTool<S, E> {
    kind: ProjectRestToolKind,
    backend: PlatformProjectToolsBackend<S, E>,
    spec: nenjo::ToolSpec,
}

impl<S, E> ProjectRestTool<S, E>
where
    S: ManifestReader + 'static,
    E: SensitivePayloadEncoder + Clone + Send + Sync + 'static,
{
    fn from_name(name: &str, backend: PlatformProjectToolsBackend<S, E>) -> Option<Self> {
        let kind = ProjectRestToolKind::from_name(name)?;
        Some(Self {
            kind,
            backend,
            spec: project_rest_tool_spec(kind)?,
        })
    }
}

#[async_trait]
impl<S, E> Tool for ProjectRestTool<S, E>
where
    S: ManifestReader + 'static,
    E: SensitivePayloadEncoder + Clone + Send + Sync + 'static,
{
    fn name(&self) -> &str {
        &self.spec.name
    }

    fn description(&self) -> &str {
        &self.spec.description
    }

    fn parameters_schema(&self) -> serde_json::Value {
        self.spec.parameters.clone()
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let args = self
            .backend
            .normalize_project_tool_args(args, self.kind)
            .await?;
        let output = match self.kind {
            ProjectRestToolKind::ListProjectTasks => {
                let args: ListProjectTasksArgs = parse_project_tool_args(
                    args,
                    "list_project_tasks",
                    "Expected {\"project_id\":\"<canonical 8-4-4-4-12 UUID>\"}.",
                )?;
                self.backend
                    .client
                    .list_project_tasks(&ProjectTaskListQuery {
                        project_id: args.project_id,
                        status: args.status,
                        priority: args.priority,
                        task_type: args.task_type,
                        tags: args.tags.map(|tags| tags.join(",")),
                        routine_id: args.routine_id,
                        assigned_agent_id: args.assigned_agent_id,
                        limit: args.limit,
                        offset: args.offset,
                    })
                    .await?
            }
            ProjectRestToolKind::GetProjectTask => {
                let args: GetProjectTaskArgs = parse_project_tool_args(
                    args,
                    "get_project_task",
                    "Expected {\"task_id\":\"<canonical 8-4-4-4-12 UUID>\"}.",
                )?;
                self.backend.client.get_project_task(args.task_id).await?
            }
            ProjectRestToolKind::CreateProjectTasks => {
                let args: CreateProjectTasksArgs = parse_project_tool_args(
                    args,
                    "create_project_tasks",
                    "Expected {\"project_id\":\"<canonical 8-4-4-4-12 UUID>\",\"tasks\":[{\"title\":\"...\"}]}.",
                )?;
                let body = self.backend.create_tasks_body(&args).await?;
                self.backend.client.bulk_create_project_tasks(&body).await?
            }
            ProjectRestToolKind::UpdateProjectTask => {
                let args: UpdateProjectTaskArgs = parse_project_tool_args(
                    args,
                    "update_project_task",
                    "Expected {\"task_id\":\"<canonical 8-4-4-4-12 UUID>\", ...fields}.",
                )?;
                let body = self.backend.update_task_body(&args).await?;
                self.backend
                    .client
                    .update_project_task(args.task_id, &body)
                    .await?
            }
            ProjectRestToolKind::DeleteProjectTask => {
                let args: DeleteProjectTaskArgs = parse_project_tool_args(
                    args,
                    "delete_project_task",
                    "Expected {\"task_id\":\"<canonical 8-4-4-4-12 UUID>\"}.",
                )?;
                self.backend
                    .client
                    .delete_project_task(args.task_id)
                    .await?;
                json!({ "deleted": true, "task_id": args.task_id })
            }
            ProjectRestToolKind::ListProjectExecutionRuns => {
                let args: ListProjectExecutionRunsArgs = parse_project_tool_args(
                    args,
                    "list_project_execution_runs",
                    "Expected {\"project_id\":\"<canonical 8-4-4-4-12 UUID>\"}.",
                )?;
                self.backend
                    .client
                    .list_project_execution_runs(&ProjectExecutionListQuery {
                        project_id: args.project_id,
                        agent_id: args.agent_id,
                        routine_id: args.routine_id,
                        status: args.status,
                        limit: args.limit,
                        offset: args.offset,
                    })
                    .await?
            }
            ProjectRestToolKind::GetProjectExecutionRun => {
                let args: GetProjectExecutionRunArgs = parse_project_tool_args(
                    args,
                    "get_project_execution_run",
                    "Expected {\"execution_run_id\":\"<canonical 8-4-4-4-12 UUID>\"}.",
                )?;
                self.backend
                    .client
                    .get_project_execution_run(args.execution_run_id)
                    .await?
            }
            ProjectRestToolKind::StartProjectExecution => {
                let args: StartProjectExecutionArgs = parse_project_tool_args(
                    args,
                    "start_project_execution",
                    "Expected {\"project_id\":\"<canonical 8-4-4-4-12 UUID>\"}.",
                )?;
                self.backend
                    .client
                    .create_execution_run(&CreateExecutionRequest {
                        project_id: args.project_id,
                        config: args.config.unwrap_or_else(|| json!({})),
                        model_count: args.model_count,
                        parallel_count: args.parallel_count,
                        initial_status: Some("running".to_string()),
                    })
                    .await?
            }
            ProjectRestToolKind::PauseProjectExecution => {
                let args: CommandProjectExecutionArgs = parse_project_tool_args(
                    args,
                    "pause_project_execution",
                    "Expected {\"execution_run_id\":\"<canonical 8-4-4-4-12 UUID>\"}.",
                )?;
                self.backend
                    .client
                    .command_project_execution_run(args.execution_run_id, "pause")
                    .await?
            }
            ProjectRestToolKind::ResumeProjectExecution => {
                let args: CommandProjectExecutionArgs = parse_project_tool_args(
                    args,
                    "resume_project_execution",
                    "Expected {\"execution_run_id\":\"<canonical 8-4-4-4-12 UUID>\"}.",
                )?;
                self.backend
                    .client
                    .command_project_execution_run(args.execution_run_id, "resume")
                    .await?
            }
        };

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&output)?,
            error: None,
        })
    }

    fn category(&self) -> ToolCategory {
        self.spec.category
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ListProjectTasksArgs {
    #[serde(alias = "id")]
    project_id: Uuid,
    status: Option<String>,
    priority: Option<String>,
    #[serde(rename = "type")]
    task_type: Option<String>,
    tags: Option<Vec<String>>,
    routine_id: Option<Uuid>,
    assigned_agent_id: Option<Uuid>,
    limit: Option<i64>,
    offset: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GetProjectTaskArgs {
    task_id: Uuid,
}

#[derive(Debug, Deserialize)]
#[serde(try_from = "CreateProjectTasksInput")]
struct CreateProjectTasksArgs {
    project_id: Uuid,
    tasks: Vec<CreateProjectTaskItemArgs>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(untagged)]
enum CreateProjectTasksInput {
    Bulk {
        #[serde(alias = "id")]
        project_id: Uuid,
        tasks: Vec<CreateProjectTaskItemArgs>,
    },
    SingleNested {
        #[serde(alias = "id")]
        project_id: Uuid,
        task: CreateProjectTaskItemArgs,
    },
    SingleFlat {
        #[serde(alias = "id")]
        project_id: Uuid,
        #[serde(flatten)]
        task: CreateProjectTaskItemArgs,
    },
}

impl TryFrom<CreateProjectTasksInput> for CreateProjectTasksArgs {
    type Error = String;

    fn try_from(input: CreateProjectTasksInput) -> std::result::Result<Self, Self::Error> {
        let (project_id, tasks) = match input {
            CreateProjectTasksInput::Bulk { project_id, tasks } => (project_id, tasks),
            CreateProjectTasksInput::SingleNested { project_id, task }
            | CreateProjectTasksInput::SingleFlat { project_id, task } => (project_id, vec![task]),
        };

        if tasks.is_empty() {
            return Err("tasks must contain at least one task".into());
        }

        Ok(Self { project_id, tasks })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateProjectTaskItemArgs {
    title: String,
    description: Option<String>,
    acceptance_criteria: Option<String>,
    status: Option<String>,
    priority: Option<String>,
    #[serde(rename = "type")]
    task_type: Option<String>,
    complexity: Option<i16>,
    tags: Option<Vec<String>>,
    required_tags: Option<Vec<String>>,
    order_index: Option<i32>,
    assigned_agent_id: Option<Uuid>,
    routine_id: Option<Uuid>,
    metadata: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateProjectTaskArgs {
    task_id: Uuid,
    title: Option<String>,
    description: Option<String>,
    acceptance_criteria: Option<String>,
    status: Option<String>,
    priority: Option<String>,
    #[serde(rename = "type")]
    task_type: Option<String>,
    complexity: Option<i16>,
    tags: Option<Vec<String>>,
    required_tags: Option<Vec<String>>,
    order_index: Option<i32>,
    assigned_agent_id: Option<Uuid>,
    routine_id: Option<Uuid>,
    metadata: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeleteProjectTaskArgs {
    task_id: Uuid,
}

#[derive(Debug, Deserialize)]
struct ListProjectExecutionRunsArgs {
    #[serde(alias = "id")]
    project_id: Uuid,
    agent_id: Option<Uuid>,
    routine_id: Option<Uuid>,
    status: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct GetProjectExecutionRunArgs {
    execution_run_id: Uuid,
}

#[derive(Debug, Deserialize)]
struct StartProjectExecutionArgs {
    #[serde(alias = "id")]
    project_id: Uuid,
    config: Option<serde_json::Value>,
    model_count: Option<i32>,
    parallel_count: Option<i32>,
}

#[derive(Debug, Deserialize)]
struct CommandProjectExecutionArgs {
    execution_run_id: Uuid,
}

fn project_rest_tool_spec(kind: ProjectRestToolKind) -> Option<nenjo::ToolSpec> {
    project_rest_tools()
        .into_iter()
        .find(|tool| tool.name == kind.tool_name())
}

fn parse_project_tool_args<T>(
    args: serde_json::Value,
    tool_name: &str,
    expected_shape: &str,
) -> Result<T>
where
    T: DeserializeOwned,
{
    serde_json::from_value(args.clone()).map_err(|error| {
        let received = serde_json::to_string(&args).unwrap_or_else(|_| "<unprintable>".into());
        anyhow!("invalid {tool_name} args: {error}. {expected_shape} Received: {received}")
    })
}

fn unique_near_uuid_match(raw: &str, candidates: &[Uuid]) -> Option<Uuid> {
    let normalized_raw = raw.trim().to_ascii_lowercase();
    let mut matches = candidates
        .iter()
        .copied()
        .filter(|candidate| edit_distance_at_most_one(&normalized_raw, &candidate.to_string()));

    let first = matches.next()?;
    if matches.next().is_none() {
        Some(first)
    } else {
        None
    }
}

fn edit_distance_at_most_one(left: &str, right: &str) -> bool {
    if left == right {
        return true;
    }

    let left = left.as_bytes();
    let right = right.as_bytes();
    let len_delta = left.len().abs_diff(right.len());
    if len_delta > 1 {
        return false;
    }

    let mut i = 0;
    let mut j = 0;
    let mut edits = 0;

    while i < left.len() && j < right.len() {
        if left[i] == right[j] {
            i += 1;
            j += 1;
            continue;
        }

        edits += 1;
        if edits > 1 {
            return false;
        }

        match left.len().cmp(&right.len()) {
            std::cmp::Ordering::Greater => i += 1,
            std::cmp::Ordering::Less => j += 1,
            std::cmp::Ordering::Equal => {
                i += 1;
                j += 1;
            }
        }
    }

    edits + usize::from(i < left.len() || j < right.len()) <= 1
}

impl ManifestContractTool {
    fn new(spec: nenjo::ToolSpec, backend: Arc<dyn ManifestMcpBackend>) -> Self {
        Self { spec, backend }
    }
}

fn add_manifest_tool_guidance(tool_name: &str, value: &mut serde_json::Value) {
    let Some((prompt_tool, resource_key)) = (match tool_name {
        "get_agent" => Some(("get_agent_prompt", "agent")),
        "get_ability" => Some(("get_ability_prompt", "ability")),
        "get_domain" => Some(("get_domain_prompt", "domain")),
        _ => None,
    }) else {
        return;
    };

    let Some(id) = value
        .get(resource_key)
        .and_then(|resource| resource.get("id"))
        .cloned()
    else {
        return;
    };

    if let Some(object) = value.as_object_mut() {
        object.insert(
            "_tool_guidance".to_string(),
            json!({
                "prompt_config_omitted": true,
                "prompt_config_tool": prompt_tool,
                "prompt_config_args": { "id": id },
                "message": format!(
                    "{tool_name} returns metadata only. To read prompt_config, call {prompt_tool} with the same id."
                ),
            }),
        );
    }
}

#[async_trait]
impl Tool for ManifestContractTool {
    fn name(&self) -> &str {
        &self.spec.name
    }

    fn description(&self) -> &str {
        &self.spec.description
    }

    fn parameters_schema(&self) -> serde_json::Value {
        self.spec.parameters.clone()
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let mut value =
            ManifestMcpContract::dispatch(self.backend.as_ref(), &self.spec.name, args).await?;
        add_manifest_tool_guidance(&self.spec.name, &mut value);
        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&value)?,
            error: None,
        })
    }

    fn category(&self) -> ToolCategory {
        self.spec.category
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_project_tasks_args_accept_bulk_and_single_task_shapes() {
        let project_id = Uuid::new_v4();

        let bulk: CreateProjectTasksArgs = parse_project_tool_args(
            json!({
                "project_id": project_id,
                "tasks": [
                    {
                        "title": "Bulk task",
                        "description": "Bulk task body"
                    }
                ]
            }),
            "create_project_tasks",
            "expected shape",
        )
        .unwrap();
        assert_eq!(bulk.project_id, project_id);
        assert_eq!(bulk.tasks.len(), 1);
        assert_eq!(bulk.tasks[0].title, "Bulk task");

        let nested: CreateProjectTasksArgs = parse_project_tool_args(
            json!({
                "project_id": project_id,
                "task": {
                    "title": "Nested single task"
                }
            }),
            "create_project_tasks",
            "expected shape",
        )
        .unwrap();
        assert_eq!(nested.tasks.len(), 1);
        assert_eq!(nested.tasks[0].title, "Nested single task");

        let flat: CreateProjectTasksArgs = parse_project_tool_args(
            json!({
                "project_id": project_id,
                "title": "Flat single task",
                "acceptance_criteria": "Done criteria"
            }),
            "create_project_tasks",
            "expected shape",
        )
        .unwrap();
        assert_eq!(flat.tasks.len(), 1);
        assert_eq!(flat.tasks[0].title, "Flat single task");
        assert_eq!(
            flat.tasks[0].acceptance_criteria.as_deref(),
            Some("Done criteria")
        );
    }

    #[test]
    fn project_tool_arg_errors_include_uuid_detail_and_received_args() {
        let error = parse_project_tool_args::<ListProjectTasksArgs>(
            json!({"project_id": "48e857455-ebb8-4678-8dd8-a9c1b7e9e140"}),
            "list_project_tasks",
            "Expected project_id.",
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("invalid list_project_tasks args"));
        assert!(error.contains("UUID parsing failed"));
        assert!(error.contains("Received:"));
        assert!(error.contains("48e857455-ebb8-4678-8dd8-a9c1b7e9e140"));
    }

    #[test]
    fn malformed_project_id_can_be_repaired_from_unique_cached_project_id() {
        let project_id = Uuid::parse_str("48e85745-ebb8-46f8-8dd8-a9c1b7e9e140").unwrap();
        let raw = "48e857455-ebb8-46f8-8dd8-a9c1b7e9e140";

        assert_eq!(unique_near_uuid_match(raw, &[project_id]), Some(project_id));
    }

    #[test]
    fn malformed_project_id_repair_requires_unique_match() {
        let first = Uuid::parse_str("48e85745-ebb8-46f8-8dd8-a9c1b7e9e140").unwrap();
        let second = Uuid::parse_str("48e85755-ebb8-46f8-8dd8-a9c1b7e9e140").unwrap();

        assert_eq!(
            unique_near_uuid_match("48e8575-ebb8-46f8-8dd8-a9c1b7e9e140", &[first, second]),
            None
        );
    }

    #[test]
    fn get_agent_output_guides_prompt_followup_tool() {
        let agent_id = Uuid::new_v4();
        let mut value = json!({
            "agent": {
                "id": agent_id,
                "name": "nenji"
            }
        });

        add_manifest_tool_guidance("get_agent", &mut value);

        assert_eq!(
            value["_tool_guidance"]["prompt_config_tool"],
            json!("get_agent_prompt")
        );
        assert_eq!(
            value["_tool_guidance"]["prompt_config_args"],
            json!({ "id": agent_id })
        );
        assert_eq!(
            value["_tool_guidance"]["prompt_config_omitted"],
            json!(true)
        );
    }
}
