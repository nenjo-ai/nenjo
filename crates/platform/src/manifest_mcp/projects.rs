use nenjo::{ToolCategory, ToolSpec};
use serde_json::json;

fn project_ref_schema() -> serde_json::Value {
    json!({
        "type": "string",
        "description": "The slug of the target project."
    })
}

fn doc_id_schema() -> serde_json::Value {
    json!({
        "type": "string",
        "description": "The stable slug of the target library knowledge document."
    })
}

fn pack_id_schema() -> serde_json::Value {
    json!({
        "type": "string",
        "description": "The stable slug of the org-level knowledge pack."
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
            description: "List available projects and their summary metadata when you need to discover a project slug before reading it, working with its documents, tasks, or execution runs."
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
            description: "Create a new project when you need a new project container before adding documents, tasks, or execution runs to it."
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
        ToolSpec {
            name: "create_knowledge_doc".to_string(),
            description: "Create a new org-level library knowledge document with optional metadata and outbound graph relationships. Use list_knowledge_packs to choose pack first.".to_string(),
            parameters: json!({
                "type": "object",
                "required": ["pack", "filename", "content"],
                "properties": {
                    "pack": pack_id_schema(),
                    "filename": { "type": "string", "description": "Filename to store under the library pack's docs directory." },
                    "content": { "type": "string", "description": "Full text content for the new library knowledge document." },
                    "doc": { "type": ["string", "null"], "description": "Optional stable document slug. Omit to derive one from the title or filename." },
                    "content_type": { "type": ["string", "null"], "description": "Optional MIME type such as text/markdown or application/json." },
                    "path": { "type": ["string", "null"], "description": "Optional library-relative folder path." },
                    "title": { "type": ["string", "null"], "description": "Optional display title." },
                    "kind": { "type": ["string", "null"], "description": "Optional open-ended document kind, such as guide, playbook, policy, or note." },
                    "summary": { "type": ["string", "null"], "description": "Optional concise summary for retrieval." },
                    "tags": { "type": "array", "items": { "type": "string" }, "description": "Optional retrieval tags." },
                    "related": related_schema()
                },
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
        ToolSpec {
            name: "delete_knowledge_doc".to_string(),
            description: "Delete an existing library knowledge document when you want it removed entirely.".to_string(),
            parameters: json!({
                "type": "object",
                "required": ["pack", "doc"],
                "properties": {
                    "pack": pack_id_schema(),
                    "doc": doc_id_schema()
                },
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
        ToolSpec {
            name: "update_knowledge_doc".to_string(),
            description: "Update an existing library knowledge document's content, metadata, and optionally replace its outbound graph relationships.".to_string(),
            parameters: json!({
                "type": "object",
                "required": ["pack", "doc"],
                "properties": {
                    "pack": pack_id_schema(),
                    "doc": doc_id_schema(),
                    "content": { "type": ["string", "null"], "description": "Optional full replacement text content." },
                    "filename": { "type": ["string", "null"], "description": "Optional replacement filename." },
                    "path": { "type": ["string", "null"], "description": "Optional replacement library-relative folder path. Use null to clear." },
                    "title": { "type": ["string", "null"], "description": "Optional replacement display title. Use null to clear." },
                    "kind": { "type": ["string", "null"], "description": "Optional replacement document kind. Use null to clear." },
                    "summary": { "type": ["string", "null"], "description": "Optional replacement summary. Use null to clear." },
                    "tags": { "type": "array", "items": { "type": "string" }, "description": "Optional full replacement tag list." },
                    "related": related_schema()
                },
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
    ]
}

fn related_schema() -> serde_json::Value {
    json!({
        "type": "array",
        "description": "Optional full outbound relationship list. On update, providing this replaces existing outbound edges for the document.",
        "items": {
            "type": "object",
            "required": ["target_doc", "type"],
            "properties": {
                "target_doc": doc_id_schema(),
                "type": {
                    "type": "string",
                    "description": "Relationship type such as references, depends_on, defines, part_of, extends, or related_to."
                },
                "note": {
                    "type": ["string", "null"],
                    "description": "Optional private authoring note for the relationship."
                }
            },
            "additionalProperties": false
        }
    })
}
