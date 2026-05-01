use nenjo::{ToolCategory, ToolSpec};

fn council_id_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "string",
        "format": "uuid",
        "description": "The unique id of the target council."
    })
}

fn council_member_create_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "required": ["agent_id"],
        "properties": {
            "agent_id": {
                "type": "string",
                "format": "uuid",
                "description": "Agent id for this council member."
            },
            "priority": {
                "type": "integer",
                "description": "Member priority used by the council runtime when ordering members."
            },
            "config": {
                "type": "object",
                "description": "Optional member configuration passed through to the platform."
            }
        },
        "additionalProperties": false
    })
}

fn council_create_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "required": ["name", "leader_agent_id", "members"],
        "properties": {
            "name": { "type": "string", "description": "Display name for the council." },
            "description": { "type": ["string", "null"], "description": "Optional council description." },
            "leader_agent_id": {
                "type": "string",
                "format": "uuid",
                "description": "Leader agent id for the council."
            },
            "delegation_strategy": {
                "type": "string",
                "enum": ["decompose", "dynamic", "broadcast", "round_robin", "vote"],
                "description": "Council delegation strategy."
            },
            "config": {
                "type": "object",
                "description": "Optional council-level configuration passed through to the platform."
            },
            "members": {
                "type": "array",
                "description": "Initial council members. The leader should not be included here.",
                "items": council_member_create_schema()
            }
        },
        "additionalProperties": false
    })
}

fn council_update_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "description": "Partial patch for an existing council. This only updates council metadata.",
        "properties": {
            "name": { "type": "string", "description": "Replace the council name." },
            "description": { "type": ["string", "null"], "description": "Update or clear the description. Omit to leave unchanged." },
            "delegation_strategy": {
                "type": "string",
                "enum": ["decompose", "dynamic", "broadcast", "round_robin", "vote"],
                "description": "Replace the delegation strategy."
            },
            "config": {
                "type": "object",
                "description": "Replace the council configuration object."
            }
        },
        "additionalProperties": false
    })
}

fn council_member_update_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "description": "Partial patch for one existing council member identified by agent_id.",
        "properties": {
            "priority": {
                "type": "integer",
                "description": "Replace the member priority."
            },
            "config": {
                "type": "object",
                "description": "Replace the member configuration object."
            }
        },
        "additionalProperties": false
    })
}

/// Return manifest MCP tool definitions for council resources.
pub fn council_tools() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "list_councils".to_string(),
            description: "List councils so you can find a council id before reading, updating, or deleting one."
                .to_string(),
            parameters: serde_json::json!({"type": "object", "properties": {}, "additionalProperties": false}),
            category: ToolCategory::Read,
        },
        ToolSpec {
            name: "get_council".to_string(),
            description: "Get one council's name, description, delegation_strategy, leader_agent_id, config, and members by id."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["id"],
                "properties": { "id": council_id_schema() },
                "additionalProperties": false
            }),
            category: ToolCategory::Read,
        },
        ToolSpec {
            name: "create_council".to_string(),
            description: "Create one council with top-level name, optional description, leader_agent_id, delegation_strategy, optional config, and members."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["name", "leader_agent_id", "members"],
                "properties": council_create_schema()["properties"].clone(),
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
        ToolSpec {
            name: "update_council".to_string(),
            description: "Update one council's name, description, delegation_strategy, or config by id; use member tools to change membership."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["id"],
                "properties": {
                    "id": council_id_schema(),
                    "name": council_update_schema()["properties"]["name"].clone(),
                    "description": council_update_schema()["properties"]["description"].clone(),
                    "delegation_strategy": council_update_schema()["properties"]["delegation_strategy"].clone(),
                    "config": council_update_schema()["properties"]["config"].clone()
                },
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
        ToolSpec {
            name: "add_council_member".to_string(),
            description: "Add one council member by passing council_id, agent_id, and optional priority or config."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["council_id", "agent_id"],
                "properties": {
                    "council_id": council_id_schema(),
                    "agent_id": council_member_create_schema()["properties"]["agent_id"].clone(),
                    "priority": council_member_create_schema()["properties"]["priority"].clone(),
                    "config": council_member_create_schema()["properties"]["config"].clone()
                },
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
        ToolSpec {
            name: "update_council_member".to_string(),
            description: "Update one council member by council_id and agent_id using top-level priority or config fields."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["council_id", "agent_id"],
                "properties": {
                    "council_id": council_id_schema(),
                    "agent_id": {
                        "type": "string",
                        "format": "uuid",
                        "description": "Agent id for the council member being updated."
                    },
                    "priority": council_member_update_schema()["properties"]["priority"].clone(),
                    "config": council_member_update_schema()["properties"]["config"].clone()
                },
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
        ToolSpec {
            name: "remove_council_member".to_string(),
            description: "Remove one council member by council_id and agent_id.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["council_id", "agent_id"],
                "properties": {
                    "council_id": council_id_schema(),
                    "agent_id": {
                        "type": "string",
                        "format": "uuid",
                        "description": "Agent id for the council member being removed."
                    }
                },
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
        ToolSpec {
            name: "delete_council".to_string(),
            description: "Delete one council by id when you want it removed from the manifest."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["id"],
                "properties": { "id": council_id_schema() },
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
    ]
}
