use nenjo::{ToolCategory, ToolSpec};
use serde_json::json;

fn project_id_schema() -> serde_json::Value {
    json!({
        "type": "string",
        "format": "uuid",
        "description": "The unique id of the target project."
    })
}

fn document_id_schema() -> serde_json::Value {
    json!({
        "type": "string",
        "format": "uuid",
        "description": "The unique id of the target project document."
    })
}

fn project_document_lookup_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "project_id": project_id_schema(),
            "id_or_path": {
                "type": "string",
                "description": "Project doc id, relative path, filename, or project://<project_id>/... path"
            }
        },
        "required": ["project_id", "id_or_path"],
        "additionalProperties": false
    })
}

fn project_document_filter_schema(
    extra_properties: Option<serde_json::Value>,
    required: &[&str],
) -> serde_json::Value {
    let mut properties = json!({
        "project_id": project_id_schema(),
        "tags": {
            "type": "array",
            "items": { "type": "string" },
            "description": "Optional tags that all returned docs must have"
        },
        "kind": {
            "type": "string",
            "description": "Optional kind filter"
        },
        "authority": {
            "type": "string",
            "description": "Optional authority filter"
        },
        "status": {
            "type": "string",
            "description": "Optional status filter"
        },
        "path_prefix": {
            "type": "string",
            "description": "Optional relative path or project://<project_id>/ prefix"
        },
        "related_to": {
            "type": "string",
            "description": "Optional related doc id or path that returned docs must connect to"
        },
        "edge_type": {
            "type": "string",
            "description": "Optional relationship type used with related_to"
        }
    });

    if let Some(extra) = extra_properties
        && let Some(map) = properties.as_object_mut()
        && let Some(extra_map) = extra.as_object()
    {
        for (key, value) in extra_map {
            map.insert(key.clone(), value.clone());
        }
    }

    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false
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
            name: "list_project_documents".into(),
            description: "List project documents as compact metadata only. Use this to browse or filter the document set without loading full document content.".into(),
            parameters: project_document_filter_schema(None, &["project_id"]),
            category: ToolCategory::Read,
        },
        ToolSpec {
            name: "read_project_document_manifest".into(),
            description: "Read one project document's metadata only by id or path. Use this when you need title, tags, path, or other manifest fields but do not need the document body.".into(),
            parameters: project_document_lookup_schema(),
            category: ToolCategory::Read,
        },
        ToolSpec {
            name: "read_project_document".into(),
            description: "Read one full project document, including its body content, by id or path. Use this when you want the actual document text, not just metadata.".into(),
            parameters: project_document_lookup_schema(),
            category: ToolCategory::Read,
        },
        ToolSpec {
            name: "search_project_documents".into(),
            description: "Search project documents and return matches with body content. Use this when you want to inspect or quote the matching document text, not just find candidate paths.".into(),
            parameters: project_document_filter_schema(
                Some(json!({
                    "query": {
                        "type": "string",
                        "description": "Search query, path, title, tag, summary, or body text"
                    }
                })),
                &["project_id", "query"],
            ),
            category: ToolCategory::Read,
        },
        ToolSpec {
            name: "search_project_document_paths".into(),
            description: "Search project documents and return compact metadata without body content. Use this for fast discovery or navigation when you only need to identify which documents match.".into(),
            parameters: project_document_filter_schema(
                Some(json!({
                    "query": {
                        "type": "string",
                        "description": "Search query, path, title, tag, summary, or body text"
                    }
                })),
                &["project_id", "query"],
            ),
            category: ToolCategory::Read,
        },
        ToolSpec {
            name: "list_project_document_tree".into(),
            description: "List the project document tree by path. Use this when you want a filesystem-style view of the document namespace instead of a filtered search result.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "project_id": project_id_schema(),
                    "prefix": {
                        "type": "string",
                        "description": "Optional relative path or project://<project_id>/ prefix"
                    }
                },
                "required": ["project_id"],
                "additionalProperties": false
            }),
            category: ToolCategory::Read,
        },
        ToolSpec {
            name: "list_project_document_neighbors".into(),
            description: "List graph neighbors for one project document by id or path. Use this when you want related documents connected by knowledge edges such as references or depends_on.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "project_id": project_id_schema(),
                    "id_or_path": {
                        "type": "string",
                        "description": "Project doc id, relative path, filename, or project://<project_id>/... path"
                    },
                    "edge_type": {
                        "type": "string",
                        "description": "Optional relationship type filter such as references or depends_on"
                    }
                },
                "required": ["project_id", "id_or_path"],
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
            description: "Update an existing project's top-level metadata such as name, description, or repo_url. Use this to change project settings, not project documents or tasks."
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
            name: "create_project_document".to_string(),
            description: "Create a new project document with initial body content. Use this when the document does not exist yet; use update_project_document_content to change an existing document.".to_string(),
            parameters: json!({
                "type": "object",
                "required": ["project_id", "filename", "description"],
                "properties": {
                    "project_id": project_id_schema(),
                    "filename": { "type": "string", "description": "Filename to store under the project's docs directory." },
                    "description": { "type": "string", "description": "Full text description for the new document." },
                    "content_type": { "type": ["string", "null"], "description": "Optional MIME type such as text/markdown or application/json." }
                },
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
        ToolSpec {
            name: "delete_project_document".to_string(),
            description: "Delete an existing project document from the project when you want it removed entirely.".to_string(),
            parameters: json!({
                "type": "object",
                "required": ["project_id", "document_id"],
                "properties": {
                    "project_id": project_id_schema(),
                    "document_id": document_id_schema()
                },
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
        ToolSpec {
            name: "update_project_document_content".to_string(),
            description: "Replace the body content of an existing project document. Use this to change document text without creating a new document.".to_string(),
            parameters: json!({
                "type": "object",
                "required": ["project_id", "document_id", "description"],
                "properties": {
                    "project_id": project_id_schema(),
                    "document_id": document_id_schema(),
                    "description": { "type": "string", "description": "Full replacement text description for this document." }
                },
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
    ]
}
