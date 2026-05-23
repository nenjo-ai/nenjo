use nenjo::{ToolCategory, ToolSpec};
use serde_json::json;

fn slug_schema(description: &str) -> serde_json::Value {
    json!({
        "type": "string",
        "description": description
    })
}

fn task_id_schema() -> serde_json::Value {
    json!({
        "type": "string",
        "format": "uuid",
        "description": "The unique id of the target project task."
    })
}

fn execution_run_id_schema() -> serde_json::Value {
    json!({
        "type": "string",
        "format": "uuid",
        "description": "The unique id of the target project execution run."
    })
}

/// Return REST-backed project task and execution tool definitions.
pub fn project_rest_tools() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "list_project_tasks".into(),
            description: "List tasks for a project with optional filters. Use this to browse or narrow the task set before reading, updating, or deleting a specific task.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "project": slug_schema("The target project slug."),
                    "status": {"type": "string"},
                    "priority": {"type": "string"},
                    "type": {"type": "string"},
                    "tags": {"type": "array", "items": {"type": "string"}},
                    "routine": slug_schema("Optional routine slug filter."),
                    "agent": slug_schema("Optional assigned agent slug filter."),
                    "limit": {"type": "integer"},
                    "offset": {"type": "integer"}
                },
                "required": ["project"],
                "additionalProperties": false
            }),
            category: ToolCategory::Read,
        },
        ToolSpec {
            name: "get_project_task".into(),
            description: "Read one project task by id when you already know which task you want.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "task_id": task_id_schema()
                },
                "required": ["task_id"],
                "additionalProperties": false
            }),
            category: ToolCategory::Read,
        },
        ToolSpec {
            name: "create_project_tasks".into(),
            description: "Create one or more new tasks for a project in a single call. Use this for both single-task and multi-task creation by supplying a `tasks` list with one item or many.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "project": slug_schema("The target project slug."),
                    "tasks": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "title": {"type": "string"},
                                "description": {"type": "string"},
                                "acceptance_criteria": {"type": "string"},
                                "status": {"type": "string"},
                                "priority": {"type": "string"},
                                "type": {"type": "string"},
                                "complexity": {"type": "integer"},
                                "tags": {"type": "array", "items": {"type": "string"}},
                                "required_tags": {"type": "array", "items": {"type": "string"}},
                                "order_index": {"type": "integer"},
                                "agent": slug_schema("Optional assigned agent slug."),
                                "routine": slug_schema("Optional routine slug."),
                                "metadata": {"type": "object"}
                            },
                            "required": ["title"],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["project", "tasks"],
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
        ToolSpec {
            name: "update_project_task".into(),
            description: "Update an existing project task. Use this to change task state or content after creation; sensitive task content is re-encrypted automatically when needed.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "task_id": task_id_schema(),
                    "title": {"type": "string"},
                    "description": {"type": "string"},
                    "acceptance_criteria": {"type": "string"},
                    "status": {"type": "string"},
                    "priority": {"type": "string"},
                    "type": {"type": "string"},
                    "complexity": {"type": "integer"},
                    "tags": {"type": "array", "items": {"type": "string"}},
                    "required_tags": {"type": "array", "items": {"type": "string"}},
                    "order_index": {"type": "integer"},
                    "agent": slug_schema("Optional assigned agent slug."),
                    "routine": slug_schema("Optional routine slug."),
                    "metadata": {"type": "object"}
                },
                "required": ["task_id"],
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
        ToolSpec {
            name: "delete_project_task".into(),
            description: "Delete an existing project task by id when you want it removed entirely.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "task_id": task_id_schema()
                },
                "required": ["task_id"],
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
        ToolSpec {
            name: "list_project_execution_runs".into(),
            description: "List execution runs for a project, with optional filters such as agent, routine, or status. Use this to find a run before reading or controlling it.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "project": slug_schema("The target project slug."),
                    "agent": slug_schema("Optional agent slug filter."),
                    "routine": slug_schema("Optional routine slug filter."),
                    "status": {"type": "string"},
                    "limit": {"type": "integer"},
                    "offset": {"type": "integer"}
                },
                "required": ["project"],
                "additionalProperties": false
            }),
            category: ToolCategory::Read,
        },
        ToolSpec {
            name: "get_project_execution_run".into(),
            description: "Read one project execution run by id when you already know which run you want.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "execution_run_id": execution_run_id_schema()
                },
                "required": ["execution_run_id"],
                "additionalProperties": false
            }),
            category: ToolCategory::Read,
        },
        ToolSpec {
            name: "start_project_execution".into(),
            description: "Start a new execution run for a project immediately. Use this to create a fresh run, not to resume an existing paused run.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "project": slug_schema("The target project slug."),
                    "config": {"type": "object"},
                    "model_count": {"type": "integer"},
                    "parallel_count": {"type": "integer"}
                },
                "required": ["project"],
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
        ToolSpec {
            name: "pause_project_execution".into(),
            description: "Pause an existing running execution run by id.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "execution_run_id": execution_run_id_schema()
                },
                "required": ["execution_run_id"],
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
        ToolSpec {
            name: "resume_project_execution".into(),
            description: "Resume an existing paused execution run by id. Use this instead of start_project_execution when the run already exists.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "execution_run_id": execution_run_id_schema()
                },
                "required": ["execution_run_id"],
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
    ]
}
