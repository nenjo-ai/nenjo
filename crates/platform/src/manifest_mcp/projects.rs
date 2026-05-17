use nenjo::{ToolCategory, ToolSpec};
use serde_json::json;

fn project_id_schema() -> serde_json::Value {
    json!({
        "type": "string",
        "format": "uuid",
        "description": "The unique id of the target project."
    })
}

fn item_id_schema() -> serde_json::Value {
    json!({
        "type": "string",
        "format": "uuid",
        "description": "The unique id of the target library knowledge item."
    })
}

fn pack_id_schema() -> serde_json::Value {
    json!({
        "type": "string",
        "format": "uuid",
        "description": "The unique id of the org-level knowledge pack that owns this library knowledge item."
    })
}

fn project_create_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "required": ["name"],
        "properties": {
            "name": { "type": "string", "description": "Project name." },
            "description": { "type": ["string", "null"], "description": "Optional project description." },
            "repo_url": { "type": ["string", "null"], "description": "Optional repository URL to store in the project settings on creation." }
        },
        "additionalProperties": false
    })
}

fn project_update_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "description": "Partial patch for an existing project. Omit fields you do not want to change.",
        "properties": {
            "name": { "type": "string", "description": "Replace the project name." },
            "description": { "type": ["string", "null"], "description": "Update or clear the description. Omit to leave unchanged." },
            "repo_url": { "type": ["string", "null"], "description": "Update or clear the repository URL stored in project settings. Omit to leave unchanged." }
        },
        "additionalProperties": false
    })
}

/// Return manifest MCP tool definitions for project resources.
pub fn project_tools() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "list_projects".to_string(),
            description: "List available projects and their summary metadata when you need to discover a project id before reading it, working with its documents, tasks, or execution runs."
                .to_string(),
            parameters: json!({"type": "object", "properties": {}, "additionalProperties": false}),
            category: ToolCategory::Read,
        },
        ToolSpec {
            name: "get_project".to_string(),
            description: "Read one project's full metadata by id when you already know which project you want and need its slug, description, or settings."
                .to_string(),
            parameters: json!({
                "type": "object",
                "required": ["id"],
                "properties": { "id": project_id_schema() },
                "additionalProperties": false
            }),
            category: ToolCategory::Read,
        },
        ToolSpec {
            name: "create_project".to_string(),
            description: "Create a new project when you need a new project container before adding documents, tasks, or execution runs to it."
                .to_string(),
            parameters: json!({
                "type": "object",
                "required": ["name"],
                "properties": project_create_schema()["properties"].clone(),
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
        ToolSpec {
            name: "update_project".to_string(),
            description: "Update an existing project's top-level metadata such as name, description, or repo_url. Use this to change project settings, not library knowledge items or tasks."
                .to_string(),
            parameters: json!({
                "type": "object",
                "required": ["id"],
                "properties": {
                    "id": project_id_schema(),
                    "name": project_update_schema()["properties"]["name"].clone(),
                    "description": project_update_schema()["properties"]["description"].clone(),
                    "repo_url": project_update_schema()["properties"]["repo_url"].clone()
                },
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
        ToolSpec {
            name: "delete_project".to_string(),
            description: "Delete a project by id when you want to remove the entire project record."
                .to_string(),
            parameters: json!({
                "type": "object",
                "required": ["id"],
                "properties": { "id": project_id_schema() },
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
        ToolSpec {
            name: "create_knowledge_item".to_string(),
            description: "Create a new org-level library knowledge item in an existing knowledge pack. Use list_knowledge_packs to choose pack_id first. Do not create project-scoped packs for project context.".to_string(),
            parameters: json!({
                "type": "object",
                "required": ["pack_id", "filename", "content"],
                "properties": {
                    "pack_id": pack_id_schema(),
                    "filename": { "type": "string", "description": "Filename to store under the library pack's docs directory." },
                    "content": { "type": "string", "description": "Full text content for the new library knowledge item." },
                    "content_type": { "type": ["string", "null"], "description": "Optional MIME type such as text/markdown or application/json." }
                },
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
        ToolSpec {
            name: "delete_knowledge_item".to_string(),
            description: "Delete an existing library knowledge item when you want it removed entirely.".to_string(),
            parameters: json!({
                "type": "object",
                "required": ["pack_id", "item_id"],
                "properties": {
                    "pack_id": pack_id_schema(),
                    "item_id": item_id_schema()
                },
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
        ToolSpec {
            name: "update_knowledge_item_content".to_string(),
            description: "Replace the body content of an existing library knowledge item. Use this to change item text without creating a new item.".to_string(),
            parameters: json!({
                "type": "object",
                "required": ["pack_id", "item_id", "content"],
                "properties": {
                    "pack_id": pack_id_schema(),
                    "item_id": item_id_schema(),
                    "content": { "type": "string", "description": "Full replacement text content for this library knowledge item." }
                },
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
    ]
}
