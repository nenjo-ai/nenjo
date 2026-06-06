use nenjo::{ToolCategory, ToolSpec};

fn routine_ref_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "string",
        "description": "Routine slug."
    })
}

fn routine_step_schema() -> serde_json::Value {
    serde_json::json!({
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
    serde_json::json!({
        "type": "object",
        "required": ["source_step", "target_step", "condition"],
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
                "description": "Routing condition for this edge."
            },
            "metadata": {
                "type": "object",
                "description": "Optional edge metadata such as max_attempts or on_exhausted for gate failure retry edges.",
                "additionalProperties": true
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
            "entry_steps": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Ordered set of step slugs that should act as entry points for this graph."
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
            description: "List routines so you can find a routine slug before reading, updating, or deleting one."
                .to_string(),
            parameters: serde_json::json!({"type": "object", "properties": {}, "additionalProperties": false}),
            category: ToolCategory::Read,
        },
        ToolSpec {
            name: "get_routine".to_string(),
            description: "Get one routine's name, description, trigger, metadata, steps, and edges by slug."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["routine"],
                "properties": { "routine": routine_ref_schema() },
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
            description: "Update one routine's name, description, trigger, metadata, and optionally replace its full workflow graph by slug."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["routine"],
                "properties": {
                    "routine": routine_ref_schema(),
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
            description: "Delete one routine by slug when you want it removed from the manifest."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["routine"],
                "properties": { "routine": routine_ref_schema() },
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
    ]
}
