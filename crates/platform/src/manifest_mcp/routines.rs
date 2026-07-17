use nenjo::{ToolCategory, ToolSpec};
use serde_json::{Map, Value, json};

fn routine_ref_schema() -> serde_json::Value {
    json!({
        "type": "string",
        "description": "Routine slug."
    })
}

fn routine_step_config_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "description": "Step-specific configuration payload. Supported fields are instructions and metadata only. Put step guidance in instructions. Put optional structured context under metadata. Do not put retry budgets, inputs, evaluation_criteria, or other execution controls here; retry budgets belong on on_fail edge metadata.max_attempts.",
        "properties": {
            "instructions": {
                "type": "string",
                "description": "Step-specific task instructions for agent and gate steps. Describe the local objective, inputs or upstream evidence to inspect, expected output, and pass/fail standard when applicable."
            },
            "metadata": {
                "type": ["object", "array", "string"],
                "description": "Optional JSON context rendered through {{ routine.step.metadata }}. Use this for data the step prompt explicitly references; it does not control execution."
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
        "description": "Routine graphs must be acyclic after removing on_fail edges. source_step, target_step, and condition are top-level edge fields, not metadata fields. Use on_fail only from gate steps for failure recovery, retry loops, or remediation paths; always and on_pass edges must not create cycles.",
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
                "description": "Optional edge metadata. Every edge leaving an agent or gate step must define handoff_schema: the JSON Schema contract for the target-specific payload passed through route_next_steps. Use purpose to explain why the route exists. Use handoff_instructions to tell the source agent what information to pass to this target. For an on_fail retry edge from a gate step, use max_attempts to bound retries; retry exhaustion fails the routine directly.",
                "properties": {
                    "purpose": {
                        "type": "string",
                        "description": "Why this route exists."
                    },
                    "handoff_schema": {
                        "type": "object",
                        "required": ["type"],
                        "properties": {
                            "type": {
                                "type": "string",
                                "enum": ["object"],
                                "description": "Required root JSON Schema type. It must be object."
                            },
                            "properties": {
                                "type": "object",
                                "description": "Properties of the handoff payload."
                            },
                            "required": {
                                "type": "array",
                                "items": { "type": "string" },
                                "description": "Payload properties required by the downstream step."
                            },
                            "additionalProperties": {
                                "type": "boolean",
                                "description": "Whether handoff payload fields outside properties are allowed."
                            }
                        },
                        "additionalProperties": true,
                        "description": "Required for every edge whose source step is agent or gate. A runtime-enforced JSON Schema object for the handoff payload; its root type must be object. Keep this object inside metadata.handoff_schema; purpose, handoff_instructions, and max_attempts are sibling fields in metadata. For example {\"type\":\"object\",\"required\":[\"work\"],\"properties\":{\"work\":{\"type\":\"string\"}},\"additionalProperties\":false}."
                    },
                    "handoff_instructions": {
                        "type": "string",
                        "description": "Instructions to the source agent for what to include in the target-specific route_next_steps handoff."
                    },
                    "max_attempts": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Retry budget for gate on_fail retry edges."
                    }
                },
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

fn routine_graph_field_schema(description: &str) -> Value {
    let mut schema = routine_graph_schema();
    schema["description"] = Value::String(format!(
        "{description} Pass graph as a JSON object with entry_steps, steps, and edges; do not serialize that object into a string."
    ));
    schema
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
            "is_active": {
                "type": "boolean",
                "description": "Whether the routine is active."
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
        "routine".into(),
        json!({
            "type": "string",
            "description": "Stable routine slug. Use it to create or update the same routine idempotently; omit only when you want the slug derived from metadata.name."
        }),
    );
    properties.insert("metadata".into(), configure_metadata_schema());
    properties.insert(
        "runtime_metadata".into(),
        routine_metadata_schema("Full replacement runtime metadata. Omit to leave unchanged."),
    );
    properties.insert(
        "graph".into(),
        routine_graph_field_schema("Full replacement workflow graph. Omit to leave unchanged."),
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
            description: "Get one routine's name, description, metadata, steps, and edges by slug."
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
            description: "Create or update one routine idempotently in a single backend-owned operation. Pass routine as the stable slug when you know it; the backend owns platform IDs. When graph is present it is a full replacement and must be a JSON object, not a string."
                .to_string(),
            parameters: routine_configure_parameters(),
            category: ToolCategory::Write,
        },
    ]
}
