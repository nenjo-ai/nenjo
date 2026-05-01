use nenjo::{ToolCategory, ToolSpec};

fn project_id_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "string",
        "format": "uuid",
        "description": "The unique id of the target project."
    })
}

fn project_create_schema() -> serde_json::Value {
    serde_json::json!({
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
    serde_json::json!({
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

pub fn project_tools() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "list_projects".to_string(),
            description: "List projects so you can find a project id before reading, updating, or deleting one."
                .to_string(),
            parameters: serde_json::json!({"type": "object", "properties": {}, "additionalProperties": false}),
            category: ToolCategory::Read,
        },
        ToolSpec {
            name: "get_project".to_string(),
            description: "Get one project's name, slug, description, and settings by id."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["id"],
                "properties": { "id": project_id_schema() },
                "additionalProperties": false
            }),
            category: ToolCategory::Read,
        },
        ToolSpec {
            name: "create_project".to_string(),
            description: "Create one project with top-level name, optional description, and optional repo_url."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["name"],
                "properties": project_create_schema()["properties"].clone(),
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
        ToolSpec {
            name: "update_project".to_string(),
            description: "Update one project's name, description, or repo_url by id using top-level fields."
                .to_string(),
            parameters: serde_json::json!({
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
            description: "Delete one project by id when you want it removed from the manifest."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["id"],
                "properties": { "id": project_id_schema() },
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
    ]
}
