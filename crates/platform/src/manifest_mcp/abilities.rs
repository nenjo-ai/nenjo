use nenjo::{ToolCategory, ToolSpec};

fn ability_ref_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "string",
        "description": "The target ability slug."
    })
}

fn slug_list_schema(description: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "array",
        "description": description,
        "items": {
            "type": "string"
        }
    })
}

fn ability_create_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "required": ["name", "prompt_config"],
        "properties": {
            "name": { "type": "string", "description": "The stable slug identifier for this ability." },
            "path": { "type": "string", "description": "Folder path for this ability. Omit for the root folder." },
            "description": { "type": ["string", "null"], "description": "Optional description of what the ability does." },
            "activation_condition": { "type": "string", "description": "Condition text that tells the agent when this ability should be invoked." },
            "prompt_config": {
                "type": "object",
                "required": ["developer_prompt"],
                "properties": {
                    "developer_prompt": { "type": "string", "description": "Developer prompt executed when this ability is invoked." }
                },
                "additionalProperties": false
            },
            "mcp_servers": slug_list_schema("MCP server slugs available while this ability runs."),
        },
        "additionalProperties": false
    })
}

fn ability_update_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "description": "Partial patch for an existing ability. Omit fields you do not want to change.",
        "properties": {
            "name": { "type": "string", "description": "Update the stable slug identifier." },
            "description": { "type": ["string", "null"], "description": "Update or clear the description." },
            "activation_condition": { "type": "string", "description": "Replace the activation condition text." },
            "mcp_servers": slug_list_schema("Full replacement MCP server slug assignment list for this ability. Use this field only when the user explicitly asks to change MCP assignments."),
        },
        "additionalProperties": false
    })
}

fn ability_prompt_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "required": ["developer_prompt"],
        "properties": {
            "developer_prompt": {
                "type": "string",
                "description": "Developer prompt executed when this ability is invoked."
            }
        },
        "additionalProperties": false
    })
}

/// Return manifest MCP tool definitions for ability resources.
pub fn ability_tools() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "list_abilities".to_string(),
            description: "List abilities so you can find an ability name before reading, updating, or deleting one."
                .to_string(),
            parameters: serde_json::json!({"type": "object", "properties": {}, "additionalProperties": false}),
            category: ToolCategory::Read,
        },
        ToolSpec {
            name: "get_ability".to_string(),
            description: "Get one ability's name, path, description, activation_condition, platform_scopes, and mcp_servers by ability slug."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["ability"],
                "properties": { "ability": ability_ref_schema() },
                "additionalProperties": false
            }),
            category: ToolCategory::Read,
        },
        ToolSpec {
            name: "get_ability_prompt".to_string(),
            description: "Get one ability's prompt_config by ability slug.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["ability"],
                "properties": { "ability": ability_ref_schema() },
                "additionalProperties": false
            }),
            category: ToolCategory::Read,
        },
        ToolSpec {
            name: "create_ability".to_string(),
            description: "Create one ability using the provided top-level fields and prompt_config.developer_prompt that will run when the ability is invoked."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["name", "prompt_config"],
                "properties": ability_create_schema()["properties"].clone(),
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
        ToolSpec {
            name: "update_ability".to_string(),
            description: "Update one ability by ability slug. For normal metadata edits, send only the requested metadata fields such as name or description. Use update_ability_prompt for prompt_config changes."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["ability"],
                "properties": {
                    "ability": ability_ref_schema(),
                    "name": ability_update_schema()["properties"]["name"].clone(),
                    "description": ability_update_schema()["properties"]["description"].clone(),
                    "activation_condition": ability_update_schema()["properties"]["activation_condition"].clone(),
                    "mcp_servers": ability_update_schema()["properties"]["mcp_servers"].clone()
                },
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
        ToolSpec {
            name: "update_ability_prompt".to_string(),
            description: "Update one ability's prompt_config by ability slug using prompt_config.developer_prompt."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["ability", "prompt_config"],
                "properties": {
                    "ability": ability_ref_schema(),
                    "prompt_config": ability_prompt_schema()
                },
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
        ToolSpec {
            name: "delete_ability".to_string(),
            description: "Delete one ability by ability slug when you want it removed from the manifest."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["ability"],
                "properties": { "ability": ability_ref_schema() },
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
    ]
}
