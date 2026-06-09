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
                "description": "Optional edge metadata. For an on_fail retry edge from a gate step, max_attempts and on_exhausted_step_id can be used to bound retries and route after exhaustion.",
                "additionalProperties": true
            }
        },
        "additionalProperties": false
    })
}

fn routine_graph_schema() -> serde_json::Value {
    json!({
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
            }
        },
        "description": description,
        "additionalProperties": false
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

fn routine_common_properties(
    description_schema: Value,
    trigger_description: &str,
    metadata_description: &str,
    graph_description: &str,
) -> Map<String, Value> {
    let mut properties = Map::new();
    properties.insert("description".into(), description_schema);
    properties.insert(
        "trigger".into(),
        routine_trigger_schema(trigger_description),
    );
    properties.insert(
        "metadata".into(),
        routine_metadata_schema(metadata_description),
    );
    properties.insert(
        "graph".into(),
        routine_graph_field_schema(graph_description),
    );
    properties
}

fn routine_create_properties() -> Map<String, Value> {
    let mut properties = Map::new();
    properties.insert(
        "name".into(),
        json!({ "type": "string", "description": "Routine name." }),
    );
    properties.extend(routine_common_properties(
        json!({ "type": ["string", "null"], "description": "Optional routine description." }),
        "Routine trigger type. Use `task` for task-driven routines or `cron` for scheduled routines.",
        "Routine runtime metadata such as cron schedule.",
        "Optional full workflow graph to create along with the routine.",
    ));
    properties
}

fn routine_update_properties() -> Map<String, Value> {
    let mut properties = Map::new();
    properties.insert(
        "name".into(),
        json!({ "type": "string", "description": "Replace the routine name. The stored slug will be derived from the new name." }),
    );
    properties.extend(routine_common_properties(
        json!({ "type": ["string", "null"], "description": "Update or clear the description. Omit to leave unchanged." }),
        "Replace the routine trigger type.",
        "Replace the routine metadata object.",
        "Optional full replacement workflow graph.",
    ));
    properties
}

fn routine_create_parameters() -> Value {
    json!({
        "type": "object",
        "required": ["name"],
        "properties": routine_create_properties(),
        "additionalProperties": false
    })
}

fn routine_update_parameters() -> Value {
    let mut properties = Map::new();
    properties.insert("slug".into(), routine_ref_schema());
    properties.extend(routine_update_properties());

    json!({
        "type": "object",
        "required": ["slug"],
        "properties": properties,
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
            name: "create_routine".to_string(),
            description: "Create one routine with a name, optional description, trigger, metadata, and optionally a full workflow graph. The stable routine slug is derived from the name and returned in the response."
                .to_string(),
            parameters: routine_create_parameters(),
            category: ToolCategory::Write,
        },
        ToolSpec {
            name: "update_routine".to_string(),
            description: "Update one routine's name, description, trigger, metadata, and optionally replace its full workflow graph by slug."
                .to_string(),
            parameters: routine_update_parameters(),
            category: ToolCategory::Write,
        },
        ToolSpec {
            name: "delete_routine".to_string(),
            description: "Delete one routine by slug when you want it removed from the manifest."
                .to_string(),
            parameters: json!({
                "type": "object",
                "required": ["slug"],
                "properties": { "slug": routine_ref_schema() },
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
    ]
}
