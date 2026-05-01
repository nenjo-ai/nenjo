use nenjo::{ToolCategory, ToolSpec};

fn routine_id_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "string",
        "format": "uuid",
        "description": "The unique id of the target routine."
    })
}

fn routine_step_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "required": ["step_id", "name", "step_type", "order_index"],
        "properties": {
            "step_id": {
                "type": "string",
                "description": "Stable step identifier within this graph payload. Edges and entry_step_ids must reference these values."
            },
            "name": {
                "type": "string",
                "description": "Human-readable step name."
            },
            "step_type": {
                "type": "string",
                "enum": ["agent", "council", "cron", "gate", "terminal", "terminal_fail"],
                "description": "Execution kind for this step."
            },
            "council_id": {
                "type": ["string", "null"],
                "format": "uuid",
                "description": "Council id for council steps."
            },
            "agent_id": {
                "type": ["string", "null"],
                "format": "uuid",
                "description": "Agent id for agent steps."
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
    serde_json::json!({
        "type": "object",
        "required": ["source_step_id", "target_step_id", "condition"],
        "properties": {
            "source_step_id": {
                "type": "string",
                "description": "Source step id. Must reference one of the provided steps."
            },
            "target_step_id": {
                "type": "string",
                "description": "Target step id. Must reference one of the provided steps."
            },
            "condition": {
                "type": "string",
                "enum": ["always", "on_pass", "on_fail"],
                "description": "Routing condition for this edge."
            }
        },
        "additionalProperties": false
    })
}

fn routine_graph_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "required": ["steps", "edges"],
        "properties": {
            "entry_step_ids": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Ordered set of step_ids that should act as entry points for this graph."
            },
            "steps": {
                "type": "array",
                "description": "Full routine step list for this graph.",
                "items": routine_step_schema()
            },
            "edges": {
                "type": "array",
                "description": "Full routine edge list for this graph.",
                "items": routine_edge_schema()
            }
        },
        "additionalProperties": false
    })
}

fn routine_create_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "required": ["name"],
        "properties": {
            "name": { "type": "string", "description": "Routine name." },
            "description": { "type": ["string", "null"], "description": "Optional routine description." },
            "trigger": {
                "type": "string",
                "enum": ["task", "cron"],
                "description": "Routine trigger type. Use `task` for task-driven routines or `cron` for scheduled routines."
            },
            "metadata": {
                "type": "object",
                "description": "Routine runtime metadata such as cron schedule.",
                "properties": {
                    "schedule": {
                        "type": ["string", "null"],
                        "description": "Optional persisted cron schedule expression or interval string for routines that are meant to be scheduled."
                    }
                },
                "additionalProperties": false
            },
            "graph": {
                "description": "Optional full workflow graph to create along with the routine.",
                "allOf": [routine_graph_schema()]
            }
        },
        "additionalProperties": false
    })
}

fn routine_update_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "description": "Partial patch for an existing routine. Omit fields you do not want to change.",
        "properties": {
            "name": { "type": "string", "description": "Replace the routine name." },
            "description": { "type": ["string", "null"], "description": "Update or clear the description. Omit to leave unchanged." },
            "trigger": {
                "type": "string",
                "enum": ["task", "cron"],
                "description": "Replace the routine trigger type."
            },
            "metadata": {
                "type": "object",
                "description": "Replace the routine metadata object.",
                "properties": {
                    "schedule": {
                        "type": ["string", "null"],
                        "description": "Optional persisted cron schedule expression or interval string."
                    }
                },
                "additionalProperties": false
            },
            "graph": {
                "description": "Optional full replacement workflow graph.",
                "allOf": [routine_graph_schema()]
            }
        },
        "additionalProperties": false
    })
}

/// Return manifest MCP tool definitions for routine resources.
pub fn routine_tools() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "list_routines".to_string(),
            description: "List routines so you can find a routine id before reading, updating, or deleting one."
                .to_string(),
            parameters: serde_json::json!({"type": "object", "properties": {}, "additionalProperties": false}),
            category: ToolCategory::Read,
        },
        ToolSpec {
            name: "get_routine".to_string(),
            description: "Get one routine's name, description, trigger, metadata, steps, and edges by id."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["id"],
                "properties": { "id": routine_id_schema() },
                "additionalProperties": false
            }),
            category: ToolCategory::Read,
        },
        ToolSpec {
            name: "create_routine".to_string(),
            description: "Create one routine with name, optional description, trigger, metadata, and optionally a full workflow graph."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["name"],
                "properties": routine_create_schema()["properties"].clone(),
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
        ToolSpec {
            name: "update_routine".to_string(),
            description: "Update one routine's name, description, trigger, metadata, and optionally replace its full workflow graph by id."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["id"],
                "properties": {
                    "id": routine_id_schema(),
                    "name": routine_update_schema()["properties"]["name"].clone(),
                    "description": routine_update_schema()["properties"]["description"].clone(),
                    "trigger": routine_update_schema()["properties"]["trigger"].clone(),
                    "metadata": routine_update_schema()["properties"]["metadata"].clone(),
                    "graph": routine_update_schema()["properties"]["graph"].clone()
                },
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
        ToolSpec {
            name: "delete_routine".to_string(),
            description: "Delete one routine by id when you want it removed from the manifest."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["id"],
                "properties": { "id": routine_id_schema() },
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
    ]
}
