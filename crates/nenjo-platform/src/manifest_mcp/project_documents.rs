use nenjo::{ToolCategory, ToolSpec};

fn project_id_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "string",
        "format": "uuid",
        "description": "The unique id of the parent project."
    })
}

fn document_id_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "string",
        "format": "uuid",
        "description": "The unique id of the target project document."
    })
}

pub fn project_document_tools() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "list_project_documents".to_string(),
            description: "List one project's document ids and metadata so you can pick the right document before reading its description, creating one, or deleting one.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["project_id"],
                "properties": { "project_id": project_id_schema() },
                "additionalProperties": false
            }),
            category: ToolCategory::Read,
        },
        ToolSpec {
            name: "create_project_document".to_string(),
            description: "Create one text project document with top-level project_id, filename, description, and optional content_type.".to_string(),
            parameters: serde_json::json!({
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
            name: "get_project_document".to_string(),
            description: "Get one project document's metadata by project_id and document_id.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["project_id", "document_id"],
                "properties": {
                    "project_id": project_id_schema(),
                    "document_id": document_id_schema()
                },
                "additionalProperties": false
            }),
            category: ToolCategory::Read,
        },
        ToolSpec {
            name: "delete_project_document".to_string(),
            description: "Delete one project document by project_id and document_id when you want it removed from the project docs.".to_string(),
            parameters: serde_json::json!({
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
            name: "get_project_document_content".to_string(),
            description: "Get one project document's text description by project_id and document_id.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["project_id", "document_id"],
                "properties": {
                    "project_id": project_id_schema(),
                    "document_id": document_id_schema()
                },
                "additionalProperties": false
            }),
            category: ToolCategory::Read,
        },
        ToolSpec {
            name: "update_project_document_content".to_string(),
            description: "Replace one project document's text description by project_id and document_id using the top-level description field.".to_string(),
            parameters: serde_json::json!({
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
