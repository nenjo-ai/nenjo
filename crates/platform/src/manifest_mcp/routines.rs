use nenjo::{ToolCategory, ToolSpec};
use serde_json::{Value, json};

fn slug_schema(description: &str) -> Value {
    json!({
        "type": "string",
        "minLength": 1,
        "maxLength": 255,
        "pattern": "^[a-z0-9](?:[a-z0-9_-]{0,253}[a-z0-9])?$",
        "description": description
    })
}

fn routine_ref_schema() -> serde_json::Value {
    slug_schema("Routine slug.")
}

fn routine_step_config_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "description": "Step-specific configuration payload. Agent and gate steps use instructions and optional metadata. Human steps require request. terminal_fail steps may use failure_reason. Retry budgets belong only on gate on_fail edge max_retries.",
        "properties": {
            "instructions": {
                "type": "string",
                "description": "Step-specific task instructions for agent and gate steps. Describe the local objective, inputs or upstream evidence to inspect, expected output, and pass/fail standard when applicable."
            },
            "metadata": {
                "type": ["object", "array", "string"],
                "description": "Optional JSON context rendered through {{ routine.step.metadata }}. Use this for data the step prompt explicitly references; it does not control execution."
            },
            "request": {
                "type": "object",
                "required": ["title"],
                "description": "Required contract for human steps. The title is rendered for the reviewer. approval optionally defines structured fields collected only when the reviewer approves.",
                "properties": {
                    "title": {
                        "type": "string",
                        "minLength": 1,
                        "description": "Reviewer-facing title template, for example Review {{ task.title }}."
                    },
                    "approval": {
                        "type": "object",
                        "description": "Optional approval form contract. Follow the human-review routine documentation for field and option shapes."
                    }
                },
                "additionalProperties": false
            },
            "failure_reason": {
                "type": "string",
                "minLength": 1,
                "description": "Optional failure explanation for terminal_fail steps."
            }
        },
        "additionalProperties": false
    })
}

fn routine_step_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "required": ["slug", "name", "step_type", "order_index"],
        "properties": {
            "slug": slug_schema("Stable step slug within this routine. Edges and entry_steps must reference these values."),
            "name": {
                "type": "string",
                "minLength": 1,
                "description": "Human-readable step name."
            },
            "step_type": {
                "type": "string",
                "enum": ["agent", "council", "gate", "human", "terminal", "terminal_fail"],
                "description": "Execution kind for this step. Human steps require config.request and cannot be entry steps."
            },
            "council": {
                "type": ["string", "null"],
                "description": "Council slug for council steps."
            },
            "agent": {
                "type": ["string", "null"],
                "description": "Agent slug for agent and gate steps. Required for agent and gate steps."
            },
            "config": routine_step_config_schema(),
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
        "description": "Routine graphs must be acyclic after removing gate on_fail and human changes_requested edges. source_step, target_step, and condition are top-level edge fields, not metadata fields. Use on_fail only from gate steps and approved, changes_requested, or rejected only from human steps.",
        "properties": {
            "source_step": slug_schema("Source step slug. Must match a provided step slug."),
            "target_step": slug_schema("Target step slug. Must match a provided step slug."),
            "condition": {
                "type": "string",
                "enum": ["always", "on_pass", "on_fail", "approved", "changes_requested", "rejected"],
                "description": "Routing condition. Agent edges use always; gate edges use on_pass/on_fail; human edges use approved/changes_requested/rejected. A human outcome may fan out across multiple matching edges."
            },
            "purpose": {
                "type": "string",
                "description": "Why this route exists."
            },
            "handoff_instructions": {
                "type": "string",
                "description": "Instructions to the source agent for what to include in the target-specific route_next_steps handoff."
            },
            "handoff_schema": {
                "type": "object",
                "required": ["type"],
                "properties": {
                    "type": {
                        "type": "string",
                        "enum": ["object"]
                    },
                    "properties": {
                        "type": "object"
                    },
                    "required": {
                        "type": "array",
                        "items": { "type": "string" }
                    },
                    "additionalProperties": {
                        "type": "boolean"
                    }
                },
                "additionalProperties": true,
                "description": "Required for every edge whose source step is agent or gate. Runtime-enforced JSON Schema for the handoff payload. Artifact ID strings may use format=nenjo-artifact-id."
            },
            "max_retries": {
                "type": "integer",
                "minimum": 0,
                "description": "Retry-edge traversals after the initial gate evaluation. Allowed only on gate on_fail retry edges."
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
                "uniqueItems": true,
                "items": slug_schema("Entry step slug. Must match a provided step slug."),
                "description": "One or more step slugs that act as parallel graph entry points. A step with multiple incoming activated edges is an all-success join."
            },
            "steps": {
                "type": "array",
                "minItems": 1,
                "description": "Full routine step list for this graph.",
                "items": routine_step_schema()
            },
            "edges": {
                "type": "array",
                "description": "Full routine edge list for this graph. Cycles are allowed only through on_fail edges from gate steps or changes_requested edges from human steps; all other edge conditions must remain acyclic.",
                "items": routine_edge_schema()
            }
        },
        "additionalProperties": false
    })
}

fn routine_configure_parameters() -> Value {
    let graph = routine_graph_schema();
    json!({
        "type": "object",
        "required": ["slug", "name", "entry_steps", "steps", "edges"],
        "properties": {
            "slug": slug_schema("Required stable routine identity."),
            "name": { "type": "string", "minLength": 1 },
            "description": { "type": ["string", "null"] },
            "project_id": { "type": ["string", "null"], "format": "uuid" },
            "entry_steps": graph["properties"]["entry_steps"].clone(),
            "steps": graph["properties"]["steps"].clone(),
            "edges": graph["properties"]["edges"].clone()
        },
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
            description: "Get one routine's name, description, entry steps, steps, and edges by slug."
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
            description: "Atomically create or replace one complete routine graph by stable slug. The backend owns platform IDs."
                .to_string(),
            parameters: routine_configure_parameters(),
            category: ToolCategory::Write,
        },
    ]
}
