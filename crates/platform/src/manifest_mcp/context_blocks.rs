use nenjo::{ToolCategory, ToolSpec};

fn context_block_id_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "string",
        "format": "uuid",
        "description": "The unique id of the target context block."
    })
}

fn context_block_create_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "required": ["name", "template"],
        "properties": {
            "name": { "type": "string", "description": "Stable runtime name for this context block." },
            "path": { "type": "string", "description": "Folder path for this context block. Omit for the root folder." },
            "display_name": { "type": ["string", "null"], "description": "Optional human-friendly label for this context block." },
            "description": { "type": ["string", "null"], "description": "Optional description of what this context block injects." },
            "template": { "type": "string", "description": "MiniJinja template content for this context block." }
        },
        "additionalProperties": false
    })
}

fn context_block_update_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "description": "Partial patch for an existing context block. Omit fields you do not want to change. Use `update_context_block_content` for template changes.",
        "properties": {
            "name": { "type": "string", "description": "Rename the context block." },
            "display_name": { "type": ["string", "null"], "description": "Update or clear the display name. Omit to leave unchanged." },
            "description": { "type": ["string", "null"], "description": "Update or clear the description. Omit to leave unchanged." }
        },
        "additionalProperties": false
    })
}

/// Return manifest MCP tool definitions for context block resources.
pub fn context_block_tools() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "list_context_blocks".to_string(),
            description: "List context blocks so you can find a context block id before reading, updating, or deleting one."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
            category: ToolCategory::Read,
        },
        ToolSpec {
            name: "get_context_block".to_string(),
            description: "Get one context block's name, path, display_name, and description by id."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["id"],
                "properties": { "id": context_block_id_schema() },
                "additionalProperties": false
            }),
            category: ToolCategory::Read,
        },
        ToolSpec {
            name: "get_context_block_content".to_string(),
            description: "Get one context block's template text by id.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["id"],
                "properties": { "id": context_block_id_schema() },
                "additionalProperties": false
            }),
            category: ToolCategory::Read,
        },
        ToolSpec {
            name: "create_context_block".to_string(),
            description: "Create one context block with top-level name, optional path, display_name, description, and template."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["name", "template"],
                "properties": context_block_create_schema()["properties"].clone(),
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
        ToolSpec {
            name: "update_context_block".to_string(),
            description: "Update one context block's name, display_name, or description by id; use update_context_block_content to change template."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["id"],
                "properties": {
                    "id": context_block_id_schema(),
                    "name": context_block_update_schema()["properties"]["name"].clone(),
                    "display_name": context_block_update_schema()["properties"]["display_name"].clone(),
                    "description": context_block_update_schema()["properties"]["description"].clone()
                },
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
        ToolSpec {
            name: "update_context_block_content".to_string(),
            description: "Update one context block's template text by id using the top-level template field."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["id"],
                "properties": {
                    "id": context_block_id_schema(),
                    "template": {
                        "type": "string",
                        "description": "MiniJinja template content for this context block."
                    }
                },
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
        ToolSpec {
            name: "delete_context_block".to_string(),
            description: "Delete one context block by id when you want it removed from the manifest."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["id"],
                "properties": { "id": context_block_id_schema() },
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
    ]
}
