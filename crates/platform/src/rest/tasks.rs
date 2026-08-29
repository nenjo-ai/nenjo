//! REST tool contracts for organization tasks and task-backed execution runs.

use nenjo::{ToolCategory, ToolSpec};
use serde_json::json;

fn id(description: &str) -> serde_json::Value {
    json!({"type": "string", "format": "uuid", "description": description})
}

fn slug(description: &str) -> serde_json::Value {
    json!({"type": "string", "description": description})
}

fn recurrence() -> serde_json::Value {
    json!({
        "oneOf": [
            {
                "type": "object",
                "properties": {
                    "frequency": {"const": "interval"},
                    "every": {"type": "integer", "minimum": 1},
                    "unit": {"type": "string", "enum": ["seconds", "minutes", "hours", "days"]}
                },
                "required": ["frequency", "every", "unit"],
                "additionalProperties": false
            },
            {
                "type": "object",
                "properties": {
                    "frequency": {"const": "daily"},
                    "interval": {"type": "integer", "minimum": 1}
                },
                "required": ["frequency", "interval"],
                "additionalProperties": false
            },
            {
                "type": "object",
                "properties": {
                    "frequency": {"const": "weekly"},
                    "interval": {"type": "integer", "minimum": 1},
                    "weekdays": {
                        "type": "array",
                        "items": {"type": "string", "enum": ["monday", "tuesday", "wednesday", "thursday", "friday", "saturday", "sunday"]},
                        "minItems": 1,
                        "uniqueItems": true
                    }
                },
                "required": ["frequency", "interval", "weekdays"],
                "additionalProperties": false
            },
            {
                "type": "object",
                "properties": {
                    "frequency": {"const": "monthly"},
                    "interval": {"type": "integer", "minimum": 1},
                    "day_of_month": {"type": "integer", "minimum": 1, "maximum": 31}
                },
                "required": ["frequency", "interval", "day_of_month"],
                "additionalProperties": false
            },
            {
                "type": "object",
                "properties": {
                    "frequency": {"const": "yearly"},
                    "interval": {"type": "integer", "minimum": 1},
                    "month": {"type": "integer", "minimum": 1, "maximum": 12},
                    "day": {"type": "integer", "minimum": 1, "maximum": 31}
                },
                "required": ["frequency", "interval", "month", "day"],
                "additionalProperties": false
            },
            {
                "type": "object",
                "properties": {
                    "frequency": {"const": "cron"},
                    "expression": {"type": "string", "minLength": 1}
                },
                "required": ["frequency", "expression"],
                "additionalProperties": false
            }
        ]
    })
}

fn schedule_end() -> serde_json::Value {
    json!({
        "oneOf": [
            {
                "type": "object",
                "properties": {"type": {"const": "never"}},
                "required": ["type"],
                "additionalProperties": false
            },
            {
                "type": "object",
                "properties": {
                    "type": {"const": "on"},
                    "date": {"type": "string", "format": "date"}
                },
                "required": ["type", "date"],
                "additionalProperties": false
            },
            {
                "type": "object",
                "properties": {
                    "type": {"const": "after"},
                    "occurrences": {"type": "integer", "minimum": 1}
                },
                "required": ["type", "occurrences"],
                "additionalProperties": false
            }
        ]
    })
}

fn schedule() -> serde_json::Value {
    json!({
        "description": "Desired task schedule state. Omit to preserve the current schedule, pass null to remove it, or pass an object to create or update it. When the user asks for work to run on a schedule, explicitly include enabled: true. Creating requires enabled, starts_at, timezone, and recurrence; end defaults to never. Updating may include only changed fields.",
        "oneOf": [
            {
                "type": "object",
                "properties": {
                    "enabled": {
                        "type": "boolean",
                        "description": "Set true when the user wants the task to run on this schedule. Set false only when the user asks to pause or disable it. Omit on update only to preserve the existing active or paused state."
                    },
                    "starts_at": {
                        "type": "string",
                        "description": "Local wall-clock start in timezone, formatted as YYYY-MM-DDTHH:MM:SS without a UTC offset."
                    },
                    "timezone": {
                        "type": "string",
                        "minLength": 1,
                        "description": "IANA timezone such as America/Chicago."
                    },
                    "recurrence": recurrence(),
                    "end": schedule_end()
                },
                "additionalProperties": false
            },
            {"type": "null"}
        ]
    })
}

/// Return the task-centered tools exposed to agents with task scopes.
pub fn task_tools() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "list_tasks".into(),
            description: "List compact organization task summaries. Filters and results use human-readable slugs and catalog names; use get_task for instructions and timestamps.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "project": slug("Optional project slug."),
                    "agent": slug("Optional target agent slug."),
                    "routine": slug("Optional target routine slug."),
                    "status": {"type": "string", "description": "Optional workflow status name, such as Todo or Backlog."},
                    "label": {"type": "string", "description": "Optional label name."},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 100}
                },
                "additionalProperties": false
            }),
            category: ToolCategory::Read,
        },
        ToolSpec {
            name: "get_task".into(),
            description: "Read one task, including its decrypted instructions when this worker can decrypt them.".into(),
            parameters: json!({
                "type": "object",
                "properties": {"task_slug": slug("The task slug returned by list_tasks or configure_task.")},
                "required": ["task_slug"],
                "additionalProperties": false
            }),
            category: ToolCategory::Read,
        },
        ToolSpec {
            name: "list_task_labels".into(),
            description: "List the organization task-label catalog. Use these labels when updating a task; configure_task creates missing labels when it creates a new task.".into(),
            parameters: json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
            category: ToolCategory::Read,
        },
        ToolSpec {
            name: "list_task_execution_runs".into(),
            description: "List task-backed execution runs across the organization, optionally narrowed to one task or only active work. Active includes pending, queued, running, and paused runs.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "task_slug": slug("Optional task slug. Include project when the slug is not organization-unique."),
                    "project": slug("Optional project slug."),
                    "agent": slug("Optional target agent slug."),
                    "routine": slug("Optional target routine slug."),
                    "activity": {"type": "string", "enum": ["active"], "description": "Set to active to return only non-terminal runs. Omit for complete history."},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 200}
                },
                "additionalProperties": false
            }),
            category: ToolCategory::Read,
        },
        ToolSpec {
            name: "configure_task".into(),
            description: "Create or update one task using human-readable resource references. Before drafting, inspect labels with list_task_labels, inspect the relevant project with list_projects and get_project, and inspect the execution target with list_agents and get_agent or list_routines and get_routine so the task fits its context and capabilities. To create, omit task_slug and provide both title and non-empty instructions; missing labels are created in the organization catalog. To update, provide the exact task_slug and only the fields that should change; omit instructions to preserve the current instructions and use existing label names. For project and schedule, omit the field to preserve it or pass null to remove it. When the user asks for work to run on a schedule, include schedule.enabled=true; omitting enabled preserves an existing disabled state. A successful response is the full saved task including its current schedule, so do not repeat the same update.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "task_slug": slug("Existing task slug to update. Omit to create a task."),
                    "title": {"type": "string", "minLength": 1},
                    "instructions": {
                        "type": "string",
                        "minLength": 1,
                        "description": "Required when creating a task. Omit on update to preserve the current instructions."
                    },
                    "priority": {"type": "string", "enum": ["low", "medium", "high", "critical"]},
                    "status": {"type": "string", "description": "Existing workflow status name, such as Todo or Backlog."},
                    "project": {
                        "description": "Exact project slug returned by list_projects. Omit this field to leave the current project unchanged, or pass null to remove the project. The field name is project, not project_slug.",
                        "oneOf": [slug("Project slug."), {"type": "null"}]
                    },
                    "target": {
                        "oneOf": [
                            {"type": "object", "properties": {"type": {"const": "agent"}, "slug": slug("Agent slug.")}, "required": ["type", "slug"], "additionalProperties": false},
                            {"type": "object", "properties": {"type": {"const": "routine"}, "slug": slug("Routine slug.")}, "required": ["type", "slug"], "additionalProperties": false},
                            {"type": "null"}
                        ],
                        "description": "Agent or routine execution target, or null to clear it."
                    },
                    "schedule": schedule(),
                    "labels": {
                        "type": "array",
                        "items": {"type": "string", "minLength": 1},
                        "uniqueItems": true,
                        "description": "Complete set of organization label names. Missing labels are created when creating a new task; updates require existing names. Use an empty array to clear labels."
                    }
                },
                "allOf": [{
                    "if": {"not": {"required": ["task_slug"]}},
                    "then": {"required": ["title", "instructions"]}
                }],
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
        ToolSpec {
            name: "delete_task".into(),
            description: "Permanently delete one task and its execution history and attachments.".into(),
            parameters: json!({
                "type": "object",
                "properties": {"task_slug": slug("The task slug.")},
                "required": ["task_slug"],
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
        ToolSpec {
            name: "dispatch_task".into(),
            description: "Dispatch a manual task and return its execution_run_id immediately. Use watch_execution_run to follow progress.".into(),
            parameters: json!({
                "type": "object",
                "properties": {"task_slug": slug("The runnable manual task slug.")},
                "required": ["task_slug"],
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
        ToolSpec {
            name: "cancel_execution_run".into(),
            description: "Request cancellation of one queued or running task execution.".into(),
            parameters: json!({
                "type": "object",
                "properties": {"execution_run_id": id("The task execution run ID.")},
                "required": ["execution_run_id"],
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
        ToolSpec {
            name: "retry_execution_run".into(),
            description: "Retry one terminal task execution and return the new execution run ID.".into(),
            parameters: json!({
                "type": "object",
                "properties": {"execution_run_id": id("The terminal task execution run ID.")},
                "required": ["execution_run_id"],
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
    ]
}
