use nenjo::{ToolCategory, ToolSpec};
use serde_json::{Map, Value, json};

fn routine_ref_schema() -> serde_json::Value {
    json!({
        "type": "string",
        "description": "Routine slug."
    })
}

fn routine_step_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "required": ["slug", "name", "step_type", "order_index"],
        "properties": {
            "slug": {
                "type": "string",
                "description": "Stable step slug within this routine. Edges and entry_steps must reference these values."
            },
            "name": {
                "type": "string",
                "description": "Human-readable step name."
            },
            "step_type": {
                "type": "string",
                "enum": ["agent", "council", "gate", "terminal", "terminal_fail"],
                "description": "Execution kind for this step."
            },
            "council": {
                "type": ["string", "null"],
                "description": "Council slug for council steps."
            },
            "agent": {
                "type": ["string", "null"],
                "description": "Agent slug for agent and gate steps. Required for agent and gate steps."
            },
            "config": {
                "type": "object",
                "description": "Step-specific configuration payload.",
                "additionalProperties": true
            },
            "order_index": {
                "type": "integer",
                "description": "Display and traversal order for the step."
            }
        },
        "additionalProperties": false
    })
}

fn routine_edge_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "required": ["source_step", "target_step", "condition"],
        "description": "Routine graphs must be acyclic after removing on_fail edges. Use on_fail only from gate steps for failure recovery, retry loops, or remediation paths; always and on_pass edges must not create cycles.",
        "properties": {
            "source_step": {
                "type": "string",
                "description": "Source step slug. Must match a provided step slug."
            },
            "target_step": {
                "type": "string",
                "description": "Target step slug. Must match a provided step slug."
            },
            "condition": {
                "type": "string",
                "enum": ["always", "on_pass", "on_fail"],
                "description": "Routing condition for this edge. on_fail may only originate from gate steps."
            },
            "metadata": {
                "type": "object",
                "description": "Optional edge metadata. For an on_fail retry edge from a gate step, use max_attempts to bound retries; retry exhaustion fails the routine directly.",
                "additionalProperties": true
            }
        },
        "additionalProperties": false
    })
}

fn routine_graph_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "required": ["entry_steps", "steps", "edges"],
        "properties": {
            "entry_steps": {
                "type": "array",
                "minItems": 1,
                "items": { "type": "string" },
                "description": "One or more step slugs that act as parallel graph entry points. A step with multiple incoming activated edges is an all-success join."
            },
            "steps": {
                "type": "array",
                "description": "Full routine step list for this graph.",
                "items": routine_step_schema()
            },
            "edges": {
                "type": "array",
                "description": "Full routine edge list for this graph. Cycles are allowed only through on_fail edges from gate steps; the graph formed by always and on_pass edges must remain acyclic.",
                "items": routine_edge_schema()
            }
        },
        "additionalProperties": false
    })
}

fn routine_metadata_schema(description: &str) -> Value {
    json!({
        "type": "object",
        "properties": {
            "schedule": {
                "type": ["string", "null"],
                "description": "Optional persisted cron schedule expression or interval string."
            },
            "timezone": {
                "type": ["string", "null"],
                "description": "IANA timezone used when evaluating cron schedules."
            },
            "entry_steps": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Persisted graph entry step slugs. Prefer graph.entry_steps when replacing a graph."
            }
        },
        "description": description,
        "additionalProperties": true
    })
}

fn routine_trigger_schema(description: &str) -> Value {
    json!({
        "type": "string",
        "enum": ["task", "cron"],
        "description": description
    })
}

fn routine_graph_field_schema(description: &str) -> Value {
    json!({
        "description": description,
        "allOf": [routine_graph_schema()]
    })
}

fn cron_task_schema() -> Value {
    json!({
        "type": "object",
        "required": ["title"],
        "description": "Cron task input for cron routines. Populdates the {{task}} template var for routines",
        "properties": {
            "title": {
                "type": "string",
                "description": "Task title to encrypt for scheduled routine runs."
            },
            "description": {
                "type": "string",
                "description": "Optional task description to encrypt for scheduled routine runs."
            },
            "acceptance_criteria": {
                "type": "string",
                "description": "Optional acceptance criteria to encrypt for scheduled routine runs."
            }
        },
        "additionalProperties": false
    })
}

fn configure_metadata_schema() -> Value {
    json!({
        "type": "object",
        "description": "Routine metadata patch. Required on create because metadata.name is required when routine is omitted. On update, omitted fields are unchanged and null description clears the description.",
        "properties": {
            "name": {
                "type": "string",
                "description": "Routine display name. Required when creating a routine."
            },
            "description": {
                "type": ["string", "null"],
                "description": "Human-readable description. Omit to leave unchanged; set null to clear."
            },
            "project_id": {
                "type": ["string", "null"],
                "format": "uuid",
                "description": "Project UUID to associate with the routine. Omit to leave unchanged; set null to clear."
            },
            "trigger": routine_trigger_schema("Routine trigger type. Use task for task-driven routines or cron for scheduled routines."),
            "is_active": {
                "type": "boolean",
                "description": "Whether the routine is active. For cron routines this controls schedule enablement."
            },
            "max_retries": {
                "type": "integer",
                "minimum": 0,
                "description": "Maximum routine retry count."
            }
        },
        "additionalProperties": false
    })
}

fn routine_configure_parameters() -> Value {
    let mut properties = Map::new();
    properties.insert(
        "id".into(),
        json!({
            "type": "string",
            "format": "uuid",
            "description": "Optional routine UUID to use when creating a new routine."
        }),
    );
    properties.insert(
        "routine".into(),
        json!({
            "type": "string",
            "description": "Existing routine slug. Omit to create a new routine."
        }),
    );
    properties.insert("metadata".into(), configure_metadata_schema());
    properties.insert(
        "runtime_metadata".into(),
        routine_metadata_schema("Full replacement runtime metadata, such as cron schedule and timezone. Omit to leave unchanged."),
    );
    properties.insert(
        "graph".into(),
        routine_graph_field_schema("Full replacement workflow graph. Omit to leave unchanged."),
    );
    properties.insert("cron_task".into(), cron_task_schema());
    properties.insert(
        "encrypted_payload".into(),
        json!({
            "type": ["object", "null"],
            "description": "Optional encrypted routine payload stored by the platform.",
            "additionalProperties": true
        }),
    );

    json!({
        "type": "object",
        "properties": properties,
        "additionalProperties": false
    })
}

/// Return manifest MCP tool definitions for routine resources.
pub fn routine_tools() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "list_routines".to_string(),
            description: "List routines so you can find a routine slug before reading or configuring one."
                .to_string(),
            parameters: json!({"type": "object", "properties": {}, "additionalProperties": false}),
            category: ToolCategory::Read,
        },
        ToolSpec {
            name: "get_routine".to_string(),
            description: "Get one routine's name, description, trigger, metadata, steps, and edges by slug."
                .to_string(),
            parameters: json!({
                "type": "object",
                "required": ["slug"],
                "properties": { "slug": routine_ref_schema() },
                "additionalProperties": false
            }),
            category: ToolCategory::Read,
        },
        ToolSpec {
            name: "configure_routine".to_string(),
            description: "Create or update one routine in a single backend-owned operation. Omit routine to create; pass routine to update. When graph is present it is a full replacement and must be a JSON object, not a string."
                .to_string(),
            parameters: routine_configure_parameters(),
            category: ToolCategory::Write,
        },
    ]
}
