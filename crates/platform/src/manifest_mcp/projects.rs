use nenjo::{ToolCategory, ToolSpec};
use serde_json::json;

fn project_ref_schema() -> serde_json::Value {
    json!({
        "type": "string",
        "description": "The slug of the target project."
    })
}

fn project_create_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "required": ["name", "slug"],
        "properties": {
            "name": { "type": "string", "description": "Project name." },
            "slug": { "type": "string", "description": "User-selected project slug." },
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
            "slug": { "type": "string", "description": "Replace the project slug." },
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
            description: "List available projects and their summary metadata when you need to discover a project slug before reading it or working with its tasks and execution runs."
                .to_string(),
            parameters: json!({"type": "object", "properties": {}, "additionalProperties": false}),
            category: ToolCategory::Read,
        },
        ToolSpec {
            name: "get_project".to_string(),
            description: "Read one project's full metadata by slug when you already know which project you want and need its description or settings."
                .to_string(),
            parameters: json!({
                "type": "object",
                "required": ["project"],
                "properties": { "project": project_ref_schema() },
                "additionalProperties": false
            }),
            category: ToolCategory::Read,
        },
        ToolSpec {
            name: "create_project".to_string(),
            description: "Create a new project when you need a new project container for workspace metadata, tasks, or execution runs."
                .to_string(),
            parameters: json!({
                "type": "object",
                "required": ["name", "slug"],
                "properties": project_create_schema()["properties"].clone(),
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
        ToolSpec {
            name: "update_project".to_string(),
            description: "Update an existing project's top-level metadata such as name, description, or repo_url. Use this to change project settings, not library knowledge documents or tasks."
                .to_string(),
            parameters: json!({
                "type": "object",
                "required": ["project"],
                "properties": {
                    "project": project_ref_schema(),
                    "name": project_update_schema()["properties"]["name"].clone(),
                    "slug": project_update_schema()["properties"]["slug"].clone(),
                    "description": project_update_schema()["properties"]["description"].clone(),
                    "repo_url": project_update_schema()["properties"]["repo_url"].clone()
                },
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
        ToolSpec {
            name: "delete_project".to_string(),
            description: "Delete a project by slug when you want to remove the entire project record."
                .to_string(),
            parameters: json!({
                "type": "object",
                "required": ["project"],
                "properties": { "project": project_ref_schema() },
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
    ]
}
