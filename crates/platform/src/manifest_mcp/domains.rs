use nenjo::{ToolCategory, ToolSpec};

fn uuid_list_schema(description: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "array",
        "description": description,
        "items": {
            "type": "string",
            "format": "uuid"
        }
    })
}

fn domain_id_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "string",
        "format": "uuid",
        "description": "The unique id of the target domain."
    })
}

fn domain_create_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "required": ["name", "display_name", "command"],
        "properties": {
            "name": { "type": "string", "description": "Stable runtime name for this domain." },
            "path": { "type": "string", "description": "Folder path for this domain. Omit for the root folder." },
            "display_name": { "type": "string", "description": "Human-readable name shown in the UI." },
            "description": { "type": ["string", "null"], "description": "Optional domain description." },
            "command": { "type": "string", "description": "The slash/hash-style command used to activate this domain, such as `#creator`." },
            "ability_ids": uuid_list_schema("Ability ids activated by this domain."),
            "mcp_server_ids": uuid_list_schema("MCP server ids activated by this domain."),
            "prompt_config": {
                "type": ["object", "null"],
                "description": "Optional domain prompt configuration.",
                "required": ["developer_prompt_addon"],
                "properties": {
                    "developer_prompt_addon": { "type": ["string", "null"], "description": "Optional domain developer prompt addon text." }
                },
                "additionalProperties": false
            }
        },
        "additionalProperties": false
    })
}

fn domain_update_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "description": "Partial patch for an existing domain. Omit fields you do not want to change.",
        "properties": {
            "name": { "type": "string", "description": "Replace the runtime name." },
            "display_name": { "type": "string", "description": "Replace the human-readable display name." },
            "description": { "type": ["string", "null"], "description": "Update or clear the description. Omit to leave unchanged." },
            "command": { "type": "string", "description": "Replace the activation command for this domain." },
            "ability_ids": uuid_list_schema("Full replacement list of ability ids activated by this domain."),
            "mcp_server_ids": uuid_list_schema("Full replacement list of MCP server ids activated by this domain.")
        },
        "additionalProperties": false
    })
}

fn domain_prompt_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "required": ["developer_prompt_addon"],
        "properties": {
            "developer_prompt_addon": {
                "type": ["string", "null"],
                "description": "Domain developer prompt addon text."
            }
        },
        "additionalProperties": false
    })
}

/// Return manifest MCP tool definitions for domain resources.
pub fn domain_tools() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "list_domains".to_string(),
            description: "List domains so you can find a domain id before reading, updating, or deleting one."
                .to_string(),
            parameters: serde_json::json!({"type": "object", "properties": {}, "additionalProperties": false}),
            category: ToolCategory::Read,
        },
        ToolSpec {
            name: "get_domain".to_string(),
            description: "Get one domain's name, path, display_name, description, command, platform_scopes, ability_ids, and mcp_server_ids by id."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["id"],
                "properties": { "id": domain_id_schema() },
                "additionalProperties": false
            }),
            category: ToolCategory::Read,
        },
        ToolSpec {
            name: "get_domain_prompt".to_string(),
            description: "Get one domain's prompt_config by id."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["id"],
                "properties": { "id": domain_id_schema() },
                "additionalProperties": false
            }),
            category: ToolCategory::Read,
        },
        ToolSpec {
            name: "create_domain".to_string(),
            description: "Create one domain with top-level name, path, display_name, description, command, ability_ids, mcp_server_ids, and optional prompt_config. Domain platform scopes are managed outside this MCP tool."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["name", "display_name", "command"],
                "properties": domain_create_schema()["properties"].clone(),
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
        ToolSpec {
            name: "update_domain".to_string(),
            description: "Update one domain's name, display_name, description, command, ability_ids, or mcp_server_ids by id; use update_domain_prompt to change prompt_config. Domain platform scopes are managed outside this MCP tool."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["id"],
                "properties": {
                    "id": domain_id_schema(),
                    "name": domain_update_schema()["properties"]["name"].clone(),
                    "display_name": domain_update_schema()["properties"]["display_name"].clone(),
                    "description": domain_update_schema()["properties"]["description"].clone(),
                    "command": domain_update_schema()["properties"]["command"].clone(),
                    "ability_ids": domain_update_schema()["properties"]["ability_ids"].clone(),
                    "mcp_server_ids": domain_update_schema()["properties"]["mcp_server_ids"].clone()
                },
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
        ToolSpec {
            name: "update_domain_prompt".to_string(),
            description: "Update one domain's prompt_config by id using prompt_config.developer_prompt_addon."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["id", "prompt_config"],
                "properties": {
                    "id": domain_id_schema(),
                    "prompt_config": domain_prompt_schema()
                },
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
        ToolSpec {
            name: "delete_domain".to_string(),
            description: "Delete one domain by id when you want it removed from the manifest."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["id"],
                "properties": { "id": domain_id_schema() },
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
    ]
}
