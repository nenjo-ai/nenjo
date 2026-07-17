//! Agent-facing task CRUD, dispatch, and durable execution history.

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use nenjo::{Slug, Tool, ToolCategory, ToolOrigin, ToolResult};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::sync::Arc;
use tracing::warn;
use uuid::Uuid;

use crate::client::{
    ExecutionActivityQuery, ExecutionKindQuery, ExecutionListQuery, TaskListQuery,
};
use crate::rest::tasks::task_tools;
use crate::{
    ManifestAccessPolicy, PlatformManifestClient, PlatformResourceIdStore, PlatformResourceKind,
    ScopeResource, SensitiveContentKind, SensitivePayloadEncoder,
};

pub(crate) mod execution_responses;
mod responses;

pub use execution_responses::{
    ExecutionRunMutationResult, ExecutionRunStatus, ExecutionRunSummary, ExecutionRunTrigger,
    ExecutionRunsListResult,
};
use execution_responses::{execution_mutation_result, execution_run_summaries};
use responses::{PlatformTaskLabelRecord, PlatformTaskRecord, PlatformTaskTarget};
pub use responses::{
    ScheduledTaskState, TaskConfigureResult, TaskDispatch, TaskDocument, TaskGetResult,
    TaskLabelSummary, TaskLabelsListResult, TaskPriority, TaskSummary, TaskTarget, TasksListResult,
};

const READ_TOOLS: &[&str] = &[
    "list_tasks",
    "get_task",
    "list_task_labels",
    "list_task_execution_runs",
];
const WRITE_TOOLS: &[&str] = &[
    "configure_task",
    "delete_task",
    "dispatch_task",
    "cancel_execution_run",
    "retry_execution_run",
];

/// Dependencies for task tools exposed inside an agent harness.
pub struct PlatformTaskToolsBackend<E> {
    pub client: Arc<PlatformManifestClient>,
    pub payload_encoder: E,
    pub resource_ids: Arc<PlatformResourceIdStore>,
    pub cached_org_id: Option<Uuid>,
}

impl<E: Clone> Clone for PlatformTaskToolsBackend<E> {
    fn clone(&self) -> Self {
        Self {
            client: self.client.clone(),
            payload_encoder: self.payload_encoder.clone(),
            resource_ids: self.resource_ids.clone(),
            cached_org_id: self.cached_org_id,
        }
    }
}

/// Add task tools allowed by the API key's task scopes.
pub fn add_task_tools<E>(
    tools: &mut Vec<Arc<dyn Tool>>,
    backend: Option<PlatformTaskToolsBackend<E>>,
    policy: &ManifestAccessPolicy,
) where
    E: SensitivePayloadEncoder + Clone + Send + Sync + 'static,
{
    let Some(backend) = backend else {
        return;
    };
    if policy.can_read_resource(ScopeResource::Tasks) {
        add_named_tools(tools, backend.clone(), READ_TOOLS);
    }
    if policy.can_write_resource(ScopeResource::Tasks) {
        add_named_tools(tools, backend, WRITE_TOOLS);
    }
}

fn add_named_tools<E>(
    tools: &mut Vec<Arc<dyn Tool>>,
    backend: PlatformTaskToolsBackend<E>,
    names: &[&str],
) where
    E: SensitivePayloadEncoder + Clone + Send + Sync + 'static,
{
    for name in names {
        if tools.iter().any(|tool| tool.name() == *name) {
            continue;
        }
        if let Some(tool) = TaskTool::from_name(name, backend.clone()) {
            tools.push(Arc::new(tool));
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TaskToolKind {
    ListTasks,
    GetTask,
    ListTaskLabels,
    ListTaskExecutionRuns,
    ConfigureTask,
    DeleteTask,
    DispatchTask,
    CancelExecutionRun,
    RetryExecutionRun,
}

impl TaskToolKind {
    fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "list_tasks" => Self::ListTasks,
            "get_task" => Self::GetTask,
            "list_task_labels" => Self::ListTaskLabels,
            "list_task_execution_runs" => Self::ListTaskExecutionRuns,
            "configure_task" => Self::ConfigureTask,
            "delete_task" => Self::DeleteTask,
            "dispatch_task" => Self::DispatchTask,
            "cancel_execution_run" => Self::CancelExecutionRun,
            "retry_execution_run" => Self::RetryExecutionRun,
            _ => return None,
        })
    }

    const fn name(self) -> &'static str {
        match self {
            Self::ListTasks => "list_tasks",
            Self::GetTask => "get_task",
            Self::ListTaskLabels => "list_task_labels",
            Self::ListTaskExecutionRuns => "list_task_execution_runs",
            Self::ConfigureTask => "configure_task",
            Self::DeleteTask => "delete_task",
            Self::DispatchTask => "dispatch_task",
            Self::CancelExecutionRun => "cancel_execution_run",
            Self::RetryExecutionRun => "retry_execution_run",
        }
    }
}

struct TaskTool<E> {
    kind: TaskToolKind,
    spec: nenjo::ToolSpec,
    backend: PlatformTaskToolsBackend<E>,
}

impl<E> TaskTool<E>
where
    E: SensitivePayloadEncoder + Clone + Send + Sync + 'static,
{
    fn from_name(name: &str, backend: PlatformTaskToolsBackend<E>) -> Option<Self> {
        let kind = TaskToolKind::from_name(name)?;
        let spec = task_tools()
            .into_iter()
            .find(|spec| spec.name == kind.name())?;
        Some(Self {
            kind,
            spec,
            backend,
        })
    }
}

#[async_trait]
impl<E> Tool for TaskTool<E>
where
    E: SensitivePayloadEncoder + Clone + Send + Sync + 'static,
{
    fn name(&self) -> &str {
        &self.spec.name
    }

    fn description(&self) -> &str {
        &self.spec.description
    }

    fn parameters_schema(&self) -> Value {
        self.spec.parameters.clone()
    }

    async fn execute(&self, args: Value) -> Result<ToolResult> {
        let output = match self.kind {
            TaskToolKind::ListTasks => {
                let args: ListTasksArgs = parse_args(args, self.kind)?;
                let query = self.backend.task_list_query(args);
                let tasks = self.backend.client.list_tasks(&query).await?;
                serde_json::to_value(TasksListResult {
                    tasks: self.backend.task_summaries(tasks)?,
                })?
            }
            TaskToolKind::GetTask => {
                let args: TaskSlugArgs = parse_args(args, self.kind)?;
                match self
                    .backend
                    .client
                    .get_task(args.task_slug.as_str())
                    .await?
                {
                    Some(task) => serde_json::to_value(TaskGetResult {
                        task: Some(self.backend.task_document(task).await?),
                    })?,
                    None => serde_json::to_value(TaskGetResult { task: None })?,
                }
            }
            TaskToolKind::ListTaskLabels => {
                let _: EmptyArgs = parse_args(args, self.kind)?;
                serde_json::to_value(TaskLabelsListResult {
                    labels: task_label_summaries(self.backend.client.list_task_labels().await?)?,
                })?
            }
            TaskToolKind::ListTaskExecutionRuns => {
                let args: ListTaskExecutionRunsArgs = parse_args(args, self.kind)?;
                self.backend.list_task_execution_runs(args).await?
            }
            TaskToolKind::ConfigureTask => {
                let args: ConfigureTaskArgs = parse_args(args, self.kind)?;
                let (task, references) = self.backend.configure_task(args).await?;
                serde_json::to_value(TaskConfigureResult {
                    task: self
                        .backend
                        .task_document_with_references(task, &references)
                        .await?,
                })?
            }
            TaskToolKind::DeleteTask => {
                let args: TaskSlugArgs = parse_args(args, self.kind)?;
                let task_id = self.backend.task_id(&args.task_slug).await?;
                self.backend.client.delete_task(task_id).await?;
                json!({"deleted": true, "task_slug": args.task_slug})
            }
            TaskToolKind::DispatchTask => {
                let args: TaskSlugArgs = parse_args(args, self.kind)?;
                let task_id = self.backend.task_id(&args.task_slug).await?;
                let run = self
                    .backend
                    .client
                    .dispatch_task(task_id, Uuid::new_v4())
                    .await?;
                serde_json::to_value(execution_mutation_result(run)?)?
            }
            TaskToolKind::CancelExecutionRun => {
                let args: ExecutionRunIdArgs = parse_args(args, self.kind)?;
                let run = self
                    .backend
                    .client
                    .cancel_execution_run(args.execution_run_id)
                    .await?;
                serde_json::to_value(execution_mutation_result(run)?)?
            }
            TaskToolKind::RetryExecutionRun => {
                let args: ExecutionRunIdArgs = parse_args(args, self.kind)?;
                let run = self
                    .backend
                    .client
                    .retry_execution_run(args.execution_run_id, Uuid::new_v4())
                    .await?;
                serde_json::to_value(execution_mutation_result(run)?)?
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

    fn origin(&self) -> ToolOrigin {
        ToolOrigin::Platform
    }
}

impl<E> PlatformTaskToolsBackend<E>
where
    E: SensitivePayloadEncoder + Clone + Send + Sync + 'static,
{
    async fn org_id(&self) -> Result<Uuid> {
        match self.cached_org_id {
            Some(org_id) => Ok(org_id),
            None => self.client.current_org_id().await,
        }
    }

    fn task_list_query(&self, args: ListTasksArgs) -> TaskListQuery {
        TaskListQuery {
            project: args.project,
            status: args.status,
            label: args.label,
            agent: args.agent,
            routine: args.routine,
            limit: args.limit,
            ..TaskListQuery::default()
        }
    }

    async fn configure_task(
        &self,
        args: ConfigureTaskArgs,
    ) -> Result<(Value, ConfiguredTaskReferences)> {
        let references = ConfiguredTaskReferences::from_args(&args);
        let task_id = match (&args.task_slug, &args.instructions) {
            (Some(task_slug), Some(_)) => Some(self.task_id(task_slug).await?),
            (Some(_), None) => None,
            (None, _) => Some(Uuid::new_v4()),
        };
        let body = self.configure_task_body(args, task_id).await?;
        let task = self.client.configure_task(&body).await?;
        Ok((task, references))
    }

    async fn configure_task_body(
        &self,
        args: ConfigureTaskArgs,
        task_id: Option<Uuid>,
    ) -> Result<Value> {
        if args.task_slug.is_none()
            && args
                .instructions
                .as_deref()
                .is_none_or(|instructions| instructions.trim().is_empty())
        {
            bail!("task instructions are required when creating a task");
        }
        if args.task_slug.is_none() && matches!(&args.project, ConfigureField::Clear) {
            bail!(
                "project cannot be null when creating a task: omit project to create without one, or pass the exact project slug to assign it"
            );
        }
        let mut body = serde_json::Map::new();

        if let Some(task_slug) = args.task_slug.as_ref() {
            body.insert("task".into(), json!(task_slug));
        } else {
            let title = args
                .title
                .as_deref()
                .context("title is required when creating a task")?
                .trim();
            if title.is_empty() {
                bail!("task title cannot be empty");
            }
            let task_id = task_id.context("new task configuration requires a stable task id")?;
            body.insert("id".into(), json!(task_id));
            body.insert(
                "slug".into(),
                json!(Slug::derive(format!(
                    "{title}-{}",
                    &task_id.simple().to_string()[..8]
                ))),
            );
        }

        if let Some(title) = args.title.as_deref() {
            let title = title.trim();
            if title.is_empty() {
                bail!("task title cannot be empty");
            }
            body.insert("title".into(), json!(title));
        }
        if let Some(instructions) = args.instructions.as_deref() {
            let task_id = task_id
                .context("updating task instructions requires resolving the existing task id")?;
            body.insert("instructions".into(), Value::Null);
            body.insert(
                "encrypted_payload".into(),
                self.encode_instructions(task_id, instructions).await?,
            );
        }
        if let Some(priority) = args.priority {
            body.insert("priority".into(), serde_json::to_value(priority)?);
        }
        if let Some(status) = args.status.as_deref() {
            body.insert("status".into(), json!(status));
        }
        match args.project {
            ConfigureField::Unset => {}
            ConfigureField::Clear => {
                body.insert("project".into(), Value::Null);
            }
            ConfigureField::Set(project) => {
                body.insert("project".into(), json!(project));
            }
        }
        match args.target {
            ConfigureField::Unset => {}
            ConfigureField::Clear => {
                body.insert("target".into(), Value::Null);
            }
            ConfigureField::Set(target) => {
                body.insert("target".into(), serde_json::to_value(target)?);
            }
        }
        if let Some(labels) = args.labels {
            body.insert("labels".into(), json!(labels));
        }
        Ok(Value::Object(body))
    }

    async fn encode_instructions(&self, task_id: Uuid, instructions: &str) -> Result<Value> {
        self.payload_encoder
            .encode_payload(
                self.org_id().await?,
                task_id,
                SensitiveContentKind::TaskContent.encrypted_object_type(),
                &json!({"instructions": instructions}),
            )
            .await?
            .context("task payload encoder did not produce encrypted instructions")
    }

    async fn task(&self, task_slug: &Slug) -> Result<Value> {
        self.client
            .get_task(task_slug.as_str())
            .await?
            .with_context(|| format!("platform task not found: {task_slug}"))
    }

    async fn task_id(&self, task_slug: &Slug) -> Result<Uuid> {
        value_uuid(&self.task(task_slug).await?, "id", "task")
    }

    fn task_summaries(&self, tasks: Value) -> Result<Vec<TaskSummary>> {
        let records: Vec<PlatformTaskRecord> = serde_json::from_value(tasks)
            .context("task list response did not match the platform task contract")?;
        records
            .into_iter()
            .map(|record| self.task_summary(&record))
            .collect()
    }

    async fn task_document(&self, task: Value) -> Result<TaskDocument> {
        self.task_document_with_references(task, &ConfiguredTaskReferences::default())
            .await
    }

    async fn task_document_with_references(
        &self,
        task: Value,
        references: &ConfiguredTaskReferences,
    ) -> Result<TaskDocument> {
        let record: PlatformTaskRecord = serde_json::from_value(task)
            .context("task response did not match the platform task contract")?;
        self.reconcile_configured_references(&record, references);
        let instructions = match record.encrypted_payload.as_ref() {
            Some(encrypted) => self
                .payload_encoder
                .decode_payload(encrypted)
                .await?
                .and_then(|content| content.get("instructions").cloned())
                .map(serde_json::from_value)
                .transpose()
                .context("decrypted task instructions were not a string")?,
            None => record.instructions.clone(),
        };
        let summary = self.task_summary_with_references(&record, references)?;
        Ok(TaskDocument {
            summary,
            instructions,
            created_at: record.created_at,
            updated_at: record.updated_at,
            completed_at: record.completed_at,
        })
    }

    fn task_summary(&self, record: &PlatformTaskRecord) -> Result<TaskSummary> {
        self.task_summary_with_references(record, &ConfiguredTaskReferences::default())
    }

    fn task_summary_with_references(
        &self,
        record: &PlatformTaskRecord,
        references: &ConfiguredTaskReferences,
    ) -> Result<TaskSummary> {
        let slug = record.slug.clone();
        let project = match (
            record.project_slug.as_ref(),
            record.project_id,
            references.project.as_ref(),
        ) {
            (Some(project_slug), _, _) => Some(project_slug.clone()),
            (None, Some(_), Some(project_slug)) => Some(project_slug.clone()),
            (None, Some(id), None) => {
                Some(self.required_resource_slug(PlatformResourceKind::Project, id)?)
            }
            (None, None, _) => None,
        };
        let target = record
            .execution_target
            .map(|target| match (target, references.target.as_ref()) {
                (PlatformTaskTarget::Agent { .. }, Some(TaskTarget::Agent { slug })) => {
                    Ok(TaskTarget::Agent { slug: slug.clone() })
                }
                (PlatformTaskTarget::Routine { .. }, Some(TaskTarget::Routine { slug })) => {
                    Ok(TaskTarget::Routine { slug: slug.clone() })
                }
                (PlatformTaskTarget::Agent { .. }, Some(TaskTarget::Routine { .. }))
                | (PlatformTaskTarget::Routine { .. }, Some(TaskTarget::Agent { .. })) => {
                    bail!("platform returned a different task target kind than configure_task requested")
                }
                (PlatformTaskTarget::Agent { id }, None) => self
                    .required_resource_slug(PlatformResourceKind::Agent, id)
                    .map(|slug| TaskTarget::Agent { slug }),
                (PlatformTaskTarget::Routine { id }, None) => self
                    .required_resource_slug(PlatformResourceKind::Routine, id)
                    .map(|slug| TaskTarget::Routine { slug }),
            })
            .transpose()?;
        Ok(TaskSummary {
            slug,
            title: record.title.clone(),
            status: record.status.name.clone(),
            priority: record.priority,
            project,
            target,
            dispatch: record.dispatch,
            labels: record
                .labels
                .iter()
                .map(|label| label.name.clone())
                .collect(),
        })
    }

    /// Heal reverse-reference cache entries from slugs the platform just
    /// resolved successfully. Cache persistence is deliberately best-effort:
    /// a local cache problem must not turn a committed configure operation
    /// into a failed tool result and prompt an unsafe retry.
    fn reconcile_configured_references(
        &self,
        record: &PlatformTaskRecord,
        references: &ConfiguredTaskReferences,
    ) {
        if let (Some(id), Some(slug)) = (record.project_id, references.project.as_ref())
            && let Err(error) = self
                .resource_ids
                .upsert(PlatformResourceKind::Project, slug, id)
        {
            warn!(%error, %slug, %id, "Failed to cache configured task project reference");
        }

        let target = match (record.execution_target, references.target.as_ref()) {
            (Some(PlatformTaskTarget::Agent { id }), Some(TaskTarget::Agent { slug })) => {
                Some((PlatformResourceKind::Agent, id, slug))
            }
            (Some(PlatformTaskTarget::Routine { id }), Some(TaskTarget::Routine { slug })) => {
                Some((PlatformResourceKind::Routine, id, slug))
            }
            (Some(PlatformTaskTarget::Agent { .. }), Some(TaskTarget::Routine { .. }))
            | (Some(PlatformTaskTarget::Routine { .. }), Some(TaskTarget::Agent { .. }))
            | (None, Some(_))
            | (_, None) => None,
        };
        if let Some((kind, id, slug)) = target
            && let Err(error) = self.resource_ids.upsert(kind, slug, id)
        {
            warn!(%error, %slug, %id, resource = kind.as_str(), "Failed to cache configured task target reference");
        }
    }

    fn required_resource_slug(&self, kind: PlatformResourceKind, id: Uuid) -> Result<Slug> {
        self.resource_ids.slug_for(kind, id)?.with_context(|| {
            format!(
                "task references platform {} {id}, but its slug is missing from the worker manifest; refresh the worker manifest",
                kind.as_str()
            )
        })
    }

    async fn list_task_execution_runs(&self, args: ListTaskExecutionRunsArgs) -> Result<Value> {
        let query = ExecutionListQuery {
            task_slug: args.task_slug,
            project: args.project,
            agent: args.agent,
            routine: args.routine,
            activity: args.activity,
            kind: Some(ExecutionKindQuery::Task),
            limit: args.limit,
            ..ExecutionListQuery::default()
        };
        let runs = self.client.list_execution_runs(&query).await?;
        serde_json::to_value(ExecutionRunsListResult {
            execution_runs: execution_run_summaries(runs)?,
        })
        .context("failed to serialize compact execution run list")
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ListTasksArgs {
    project: Option<Slug>,
    agent: Option<Slug>,
    routine: Option<Slug>,
    status: Option<String>,
    label: Option<String>,
    limit: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyArgs {}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskSlugArgs {
    task_slug: Slug,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecutionRunIdArgs {
    execution_run_id: Uuid,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ListTaskExecutionRunsArgs {
    task_slug: Option<Slug>,
    project: Option<Slug>,
    agent: Option<Slug>,
    routine: Option<Slug>,
    activity: Option<ExecutionActivityQuery>,
    limit: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum TaskTargetArg {
    Agent { slug: Slug },
    Routine { slug: Slug },
}

#[derive(Debug, Clone, Default)]
struct ConfiguredTaskReferences {
    project: Option<Slug>,
    target: Option<TaskTarget>,
}

impl ConfiguredTaskReferences {
    fn from_args(args: &ConfigureTaskArgs) -> Self {
        let project = match &args.project {
            ConfigureField::Set(slug) => Some(slug.clone()),
            ConfigureField::Unset | ConfigureField::Clear => None,
        };
        let target = match &args.target {
            ConfigureField::Set(TaskTargetArg::Agent { slug }) => {
                Some(TaskTarget::Agent { slug: slug.clone() })
            }
            ConfigureField::Set(TaskTargetArg::Routine { slug }) => {
                Some(TaskTarget::Routine { slug: slug.clone() })
            }
            ConfigureField::Unset | ConfigureField::Clear => None,
        };
        Self { project, target }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigureTaskArgs {
    task_slug: Option<Slug>,
    title: Option<String>,
    instructions: Option<String>,
    priority: Option<TaskPriority>,
    status: Option<String>,
    #[serde(default)]
    project: ConfigureField<Slug>,
    #[serde(default)]
    target: ConfigureField<TaskTargetArg>,
    labels: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
enum ConfigureField<T> {
    #[default]
    Unset,
    Clear,
    Set(T),
}

impl<'de, T> Deserialize<'de> for ConfigureField<T>
where
    T: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Ok(match Option::<T>::deserialize(deserializer)? {
            Some(value) => Self::Set(value),
            None => Self::Clear,
        })
    }
}

fn value_uuid(value: &Value, field: &str, entity: &str) -> Result<Uuid> {
    let raw = value
        .get(field)
        .and_then(Value::as_str)
        .with_context(|| format!("{entity} response did not include {field}"))?;
    Uuid::parse_str(raw).with_context(|| format!("{entity} response included invalid {field}"))
}

fn task_label_summaries(value: Value) -> Result<Vec<TaskLabelSummary>> {
    let labels: Vec<PlatformTaskLabelRecord> =
        serde_json::from_value(value).context("platform returned invalid task labels")?;
    Ok(labels
        .into_iter()
        .map(|label| TaskLabelSummary {
            name: label.name,
            color: label.color,
            description: label.description,
        })
        .collect())
}

fn parse_args<T: DeserializeOwned>(args: Value, kind: TaskToolKind) -> Result<T> {
    serde_json::from_value(args).map_err(|error| anyhow!("invalid {} args: {error}", kind.name()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    use async_trait::async_trait;
    use tempfile::tempdir;

    #[derive(Clone, Default)]
    struct RecordingEncoder {
        payload: Arc<Mutex<Option<Value>>>,
        object_type: Arc<Mutex<Option<String>>>,
        decoded_payload: Arc<Mutex<Option<Value>>>,
    }

    #[async_trait]
    impl SensitivePayloadEncoder for RecordingEncoder {
        async fn encode_payload(
            &self,
            account_id: Uuid,
            object_id: Uuid,
            object_type: &str,
            payload: &Value,
        ) -> Result<Option<Value>> {
            *self.payload.lock().unwrap() = Some(payload.clone());
            *self.object_type.lock().unwrap() = Some(object_type.to_string());
            Ok(Some(json!({
                "account_id": account_id,
                "encryption_scope": "org",
                "object_id": object_id,
                "object_type": object_type,
                "algorithm": "AES-256-GCM",
                "key_version": 1,
                "nonce": "nonce",
                "ciphertext": "ciphertext"
            })))
        }

        async fn decode_payload(&self, _payload: &Value) -> Result<Option<Value>> {
            Ok(self.decoded_payload.lock().unwrap().clone())
        }
    }

    #[test]
    fn target_shape_rejects_ambiguous_target() {
        let result = serde_json::from_value::<ConfigureTaskArgs>(json!({
            "title": "Do work",
            "target": {"type": "agent", "slug": "researcher", "routine": "daily"}
        }));
        assert!(result.is_err());
    }

    #[test]
    fn configure_task_distinguishes_omitted_and_cleared_fields() {
        let omitted: ConfigureTaskArgs = serde_json::from_value(json!({
            "task_slug": "private-task-a1b2c3d4"
        }))
        .unwrap();
        assert!(matches!(omitted.project, ConfigureField::Unset));
        assert!(matches!(omitted.target, ConfigureField::Unset));

        let cleared: ConfigureTaskArgs = serde_json::from_value(json!({
            "task_slug": "private-task-a1b2c3d4",
            "project": null,
            "target": null,
            "labels": []
        }))
        .unwrap();
        assert!(matches!(cleared.project, ConfigureField::Clear));
        assert!(matches!(cleared.target, ConfigureField::Clear));
        assert_eq!(cleared.labels, Some(Vec::new()));
    }

    #[tokio::test]
    async fn create_task_rejects_null_project_with_assignment_guidance() {
        let temp = tempdir().unwrap();
        let backend = PlatformTaskToolsBackend {
            client: Arc::new(PlatformManifestClient::new("http://localhost", "test").unwrap()),
            payload_encoder: RecordingEncoder::default(),
            resource_ids: Arc::new(PlatformResourceIdStore::new(temp.path())),
            cached_org_id: Some(Uuid::new_v4()),
        };
        let args: ConfigureTaskArgs = serde_json::from_value(json!({
            "title": "Implement authentication",
            "instructions": "Implement JWT authentication for API requests.",
            "project": null
        }))
        .unwrap();

        let error = backend
            .configure_task_body(args, Some(Uuid::new_v4()))
            .await
            .unwrap_err();

        let message = error.to_string();
        assert!(message.contains("project cannot be null when creating a task"));
        assert!(message.contains("omit project"));
        assert!(message.contains("pass the exact project slug"));
    }

    #[tokio::test]
    async fn create_task_requires_non_empty_instructions() {
        let temp = tempdir().unwrap();
        let backend = PlatformTaskToolsBackend {
            client: Arc::new(PlatformManifestClient::new("http://localhost", "test").unwrap()),
            payload_encoder: RecordingEncoder::default(),
            resource_ids: Arc::new(PlatformResourceIdStore::new(temp.path())),
            cached_org_id: Some(Uuid::new_v4()),
        };

        for instructions in [None, Some("   ")] {
            let args = ConfigureTaskArgs {
                task_slug: None,
                title: Some("Implement authentication".into()),
                instructions: instructions.map(str::to_string),
                priority: None,
                status: None,
                project: ConfigureField::Unset,
                target: ConfigureField::Unset,
                labels: None,
            };
            let error = backend
                .configure_task_body(args, Some(Uuid::new_v4()))
                .await
                .unwrap_err();

            assert_eq!(
                error.to_string(),
                "task instructions are required when creating a task"
            );
        }
    }

    #[test]
    fn execution_list_filter_accepts_active_and_optional_task_slug() {
        let args: ListTaskExecutionRunsArgs = serde_json::from_value(json!({
            "task_slug": "private-task-a1b2c3d4",
            "activity": "active"
        }))
        .unwrap();
        assert_eq!(
            args.task_slug.as_ref().map(Slug::as_str),
            Some("private-task-a1b2c3d4")
        );
        assert!(matches!(
            args.activity,
            Some(ExecutionActivityQuery::Active)
        ));
    }

    #[test]
    fn task_list_uses_catalog_names_instead_of_catalog_ids() {
        let spec = task_tools()
            .into_iter()
            .find(|spec| spec.name == "list_tasks")
            .unwrap();
        let properties = spec.parameters["properties"].as_object().unwrap();
        assert!(properties.contains_key("status"));
        assert!(properties.contains_key("label"));
        assert!(!properties.contains_key("status_id"));
        assert!(!properties.contains_key("label_id"));

        let args: ListTasksArgs = serde_json::from_value(json!({
            "project": "platform",
            "agent": "builder",
            "routine": "release-check",
            "status": "Todo",
            "label": "Backend"
        }))
        .unwrap();
        let temp = tempdir().unwrap();
        let backend = PlatformTaskToolsBackend {
            client: Arc::new(PlatformManifestClient::new("http://localhost", "test").unwrap()),
            payload_encoder: RecordingEncoder::default(),
            resource_ids: Arc::new(PlatformResourceIdStore::new(temp.path())),
            cached_org_id: Some(Uuid::new_v4()),
        };
        let query = backend.task_list_query(args);
        assert_eq!(query.project.as_ref().map(Slug::as_str), Some("platform"));
        assert_eq!(query.agent.as_ref().map(Slug::as_str), Some("builder"));
        assert_eq!(
            query.routine.as_ref().map(Slug::as_str),
            Some("release-check")
        );
        assert_eq!(query.status.as_deref(), Some("Todo"));
        assert_eq!(query.label.as_deref(), Some("Backend"));
    }

    #[test]
    fn task_label_catalog_is_agent_readable_without_database_ids() {
        let spec = task_tools()
            .into_iter()
            .find(|spec| spec.name == "list_task_labels")
            .expect("task label list spec");
        assert_eq!(spec.category, ToolCategory::Read);
        assert_eq!(spec.parameters["properties"], json!({}));

        let labels = task_label_summaries(json!([{
            "id": Uuid::new_v4(),
            "org_id": Uuid::new_v4(),
            "name": "Backend",
            "normalized_name": "backend",
            "color": "#6B7280",
            "description": "Server-side work",
            "created_by": Uuid::new_v4(),
            "created_at": "2026-07-18T00:00:00Z",
            "updated_at": "2026-07-18T00:00:00Z"
        }]))
        .expect("task label summaries");
        assert_eq!(
            labels,
            [TaskLabelSummary {
                name: "Backend".into(),
                color: "#6B7280".into(),
                description: Some("Server-side work".into()),
            }]
        );
    }

    #[test]
    fn task_write_scope_exposes_label_discovery() {
        let temp = tempdir().unwrap();
        let backend = PlatformTaskToolsBackend {
            client: Arc::new(PlatformManifestClient::new("http://localhost", "test").unwrap()),
            payload_encoder: RecordingEncoder::default(),
            resource_ids: Arc::new(PlatformResourceIdStore::new(temp.path())),
            cached_org_id: Some(Uuid::new_v4()),
        };
        let mut tools = Vec::new();
        add_task_tools(
            &mut tools,
            Some(backend),
            &ManifestAccessPolicy::new(vec!["tasks:write".into()]),
        );

        assert!(tools.iter().any(|tool| tool.name() == "list_task_labels"));
        assert!(tools.iter().any(|tool| tool.name() == "configure_task"));
    }

    #[test]
    fn configure_task_guidance_explains_project_patch_semantics_and_retry_behavior() {
        let spec = task_tools()
            .into_iter()
            .find(|spec| spec.name == "configure_task")
            .unwrap();
        assert!(spec.description.contains("exact task_slug"));
        assert!(
            spec.description
                .contains("both title and non-empty instructions")
        );
        assert!(
            spec.description
                .contains("only the fields that should change")
        );
        assert!(spec.description.contains("omit instructions to preserve"));
        assert!(spec.description.contains("do not repeat the same update"));
        assert!(spec.description.contains("list_projects and get_project"));
        assert!(spec.description.contains("list_task_labels"));
        assert!(spec.description.contains("list_agents and get_agent"));
        assert!(spec.description.contains("list_routines and get_routine"));
        assert_eq!(
            spec.parameters["allOf"][0]["then"]["required"],
            json!(["title", "instructions"])
        );

        let project = spec.parameters["properties"]["project"]["description"]
            .as_str()
            .unwrap();
        assert!(project.contains("list_projects"));
        assert!(project.contains("leave the current project unchanged"));
        assert!(project.contains("pass null to remove"));
        assert!(project.contains("not project_slug"));
    }

    #[tokio::test]
    async fn create_task_encrypts_only_the_canonical_instruction_shape() {
        let temp = tempdir().unwrap();
        let encoder = RecordingEncoder::default();
        let backend = PlatformTaskToolsBackend {
            client: Arc::new(PlatformManifestClient::new("http://localhost", "test").unwrap()),
            payload_encoder: encoder.clone(),
            resource_ids: Arc::new(PlatformResourceIdStore::new(temp.path())),
            cached_org_id: Some(Uuid::new_v4()),
        };
        let task_id = Uuid::new_v4();
        let body = backend
            .configure_task_body(
                ConfigureTaskArgs {
                    task_slug: None,
                    title: Some("Private task".into()),
                    instructions: Some("Do not expose this".into()),
                    priority: None,
                    status: Some("Todo".into()),
                    project: ConfigureField::Set(Slug::parse("platform").unwrap()),
                    target: ConfigureField::Set(TaskTargetArg::Routine {
                        slug: Slug::parse("code-generation-pipeline").unwrap(),
                    }),
                    labels: Some(vec!["backend".into(), "urgent".into()]),
                },
                Some(task_id),
            )
            .await
            .unwrap();

        assert_eq!(body["id"], json!(task_id));
        assert!(body["instructions"].is_null());
        assert!(body["slug"].as_str().is_some_and(|slug| !slug.is_empty()));
        assert_eq!(body["project"], "platform");
        assert_eq!(
            body["target"],
            json!({"type": "routine", "slug": "code-generation-pipeline"})
        );
        assert_eq!(body["labels"], json!(["backend", "urgent"]));
        assert_eq!(body["status"], "Todo");
        assert!(!body.to_string().contains("Do not expose this"));
        assert_eq!(
            encoder.payload.lock().unwrap().as_ref().unwrap(),
            &json!({"instructions": "Do not expose this"})
        );
        assert_eq!(
            encoder.object_type.lock().unwrap().as_deref(),
            Some("task_content")
        );
    }

    fn platform_task(task_slug: &str, project_id: Option<Uuid>, execution_target: Value) -> Value {
        json!({
            "id": Uuid::new_v4(),
            "org_id": Uuid::new_v4(),
            "task_number": 42,
            "identifier": "TASK-42",
            "slug": task_slug,
            "project_id": project_id,
            "project_slug": null,
            "external_id": null,
            "title": "Implement authentication",
            "instructions": "Build the authentication flow",
            "encrypted_payload": null,
            "status": {
                "id": Uuid::new_v4(),
                "org_id": Uuid::new_v4(),
                "name": "Todo",
                "category": "unstarted",
                "color": "blue",
                "position": 1,
                "is_default": true,
                "created_at": "2026-07-17T00:00:00Z",
                "updated_at": "2026-07-17T00:00:00Z"
            },
            "priority": "high",
            "assignee_user_id": null,
            "execution_target": execution_target,
            "dispatch": {"mode": "manual"},
            "labels": [{
                "id": Uuid::new_v4(),
                "org_id": Uuid::new_v4(),
                "name": "Backend",
                "normalized_name": "backend",
                "color": "green",
                "description": null,
                "created_by": null,
                "created_at": "2026-07-17T00:00:00Z",
                "updated_at": "2026-07-17T00:00:00Z"
            }],
            "metadata": {},
            "created_by": Uuid::new_v4(),
            "created_at": "2026-07-17T00:00:00Z",
            "updated_at": "2026-07-17T01:00:00Z",
            "completed_at": null
        })
    }

    #[tokio::test]
    async fn task_outputs_follow_the_summary_and_document_pattern() {
        let temp = tempdir().unwrap();
        let agent_id = Uuid::new_v4();
        let project_id = Uuid::new_v4();
        let agent_slug = Slug::parse("code-generator").unwrap();
        let project_slug = Slug::parse("platform").unwrap();
        let resource_ids = Arc::new(PlatformResourceIdStore::new(temp.path()));
        resource_ids
            .upsert(PlatformResourceKind::Agent, &agent_slug, agent_id)
            .unwrap();
        resource_ids
            .upsert(PlatformResourceKind::Project, &project_slug, project_id)
            .unwrap();
        let encoder = RecordingEncoder::default();
        *encoder.decoded_payload.lock().unwrap() =
            Some(json!({"instructions": "Build the authentication flow"}));
        let backend = PlatformTaskToolsBackend {
            client: Arc::new(PlatformManifestClient::new("http://localhost", "test").unwrap()),
            payload_encoder: encoder,
            resource_ids,
            cached_org_id: Some(Uuid::new_v4()),
        };
        let mut task = platform_task(
            "implement-auth-a1b2c3d4",
            Some(project_id),
            json!({"type": "agent", "id": agent_id}),
        );
        task["instructions"] = Value::Null;
        task["encrypted_payload"] = json!({"ciphertext": "encrypted"});

        let summaries = backend.task_summaries(json!([task.clone()])).unwrap();
        assert_eq!(
            serde_json::to_value(TasksListResult { tasks: summaries }).unwrap(),
            json!({"tasks": [{
                "slug": "implement-auth-a1b2c3d4",
                "title": "Implement authentication",
                "status": "Todo",
                "priority": "high",
                "project": "platform",
                "target": {"type": "agent", "slug": "code-generator"},
                "dispatch": {"mode": "manual"},
                "labels": ["Backend"]
            }]})
        );

        let document = backend.task_document(task).await.unwrap();
        let value = serde_json::to_value(TaskGetResult {
            task: Some(document),
        })
        .unwrap();
        assert_eq!(
            value["task"]["instructions"],
            "Build the authentication flow"
        );
        assert_eq!(value["task"]["created_at"], "2026-07-17T00:00:00Z");
        assert_eq!(value["task"]["updated_at"], "2026-07-17T01:00:00Z");
        assert!(value["task"].get("id").is_none());
        assert!(value["task"].get("org_id").is_none());
        assert!(value["task"].get("project_id").is_none());
        assert!(value["task"].get("execution_target").is_none());
        assert!(value["task"].get("encrypted_payload").is_none());
        assert!(value["task"]["labels"][0].is_string());
    }

    #[test]
    fn task_summary_uses_backend_project_slug_without_manifest_cache() {
        let temp = tempdir().unwrap();
        let backend = PlatformTaskToolsBackend {
            client: Arc::new(PlatformManifestClient::new("http://localhost", "test").unwrap()),
            payload_encoder: RecordingEncoder::default(),
            resource_ids: Arc::new(PlatformResourceIdStore::new(temp.path())),
            cached_org_id: Some(Uuid::new_v4()),
        };
        let mut task = platform_task("implement-auth-a1b2c3d4", Some(Uuid::new_v4()), Value::Null);
        task["project_slug"] = json!("platform");

        let summaries = backend.task_summaries(json!([task])).unwrap();

        assert_eq!(summaries.len(), 1);
        assert_eq!(
            summaries[0].project.as_ref().map(Slug::as_str),
            Some("platform")
        );
    }

    #[tokio::test]
    async fn configured_task_uses_explicit_refs_before_manifest_cache_refresh() {
        let temp = tempdir().unwrap();
        let project_id = Uuid::new_v4();
        let routine_id = Uuid::new_v4();
        let project_slug = Slug::parse("platform").unwrap();
        let routine_slug = Slug::parse("code-generation-pipeline").unwrap();
        let resource_ids = Arc::new(PlatformResourceIdStore::new(temp.path()));
        let backend = PlatformTaskToolsBackend {
            client: Arc::new(PlatformManifestClient::new("http://localhost", "test").unwrap()),
            payload_encoder: RecordingEncoder::default(),
            resource_ids: resource_ids.clone(),
            cached_org_id: Some(Uuid::new_v4()),
        };
        let task = platform_task(
            "implement-auth-a1b2c3d4",
            Some(project_id),
            json!({"type": "routine", "id": routine_id}),
        );
        let mut task = task;
        task["project_slug"] = json!(project_slug);
        let references = ConfiguredTaskReferences {
            project: Some(project_slug.clone()),
            target: Some(TaskTarget::Routine {
                slug: routine_slug.clone(),
            }),
        };

        let document = backend
            .task_document_with_references(task, &references)
            .await
            .unwrap();

        assert_eq!(document.summary.project, Some(project_slug.clone()));
        assert_eq!(
            document.summary.target,
            Some(TaskTarget::Routine {
                slug: routine_slug.clone()
            })
        );
        assert_eq!(
            resource_ids
                .get(PlatformResourceKind::Project, &project_slug)
                .unwrap(),
            Some(project_id)
        );
        assert_eq!(
            resource_ids
                .get(PlatformResourceKind::Routine, &routine_slug)
                .unwrap(),
            Some(routine_id)
        );
    }

    #[test]
    fn task_output_fails_clearly_when_a_resource_slug_is_stale() {
        let temp = tempdir().unwrap();
        let backend = PlatformTaskToolsBackend {
            client: Arc::new(PlatformManifestClient::new("http://localhost", "test").unwrap()),
            payload_encoder: RecordingEncoder::default(),
            resource_ids: Arc::new(PlatformResourceIdStore::new(temp.path())),
            cached_org_id: Some(Uuid::new_v4()),
        };
        let missing_agent = Uuid::new_v4();
        let task = platform_task(
            "stale-task-a1b2c3d4",
            None,
            json!({"type": "agent", "id": missing_agent}),
        );

        let error = backend.task_summaries(json!([task])).unwrap_err();
        assert!(error.to_string().contains("refresh the worker manifest"));
    }
}
