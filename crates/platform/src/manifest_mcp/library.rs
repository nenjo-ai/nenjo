use nenjo::{ToolCategory, ToolSpec};
use serde_json::json;

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

/// Return manifest MCP tool definitions for Library knowledge mutations.
pub fn library_tools() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "create_knowledge_pack".to_string(),
            description: "Create a new user-managed org-level Library knowledge pack. This does not install package or GitHub-backed packs.".to_string(),
            parameters: json!({
                "type": "object",
                "required": ["name"],
                "properties": {
                    "name": { "type": "string", "description": "Human-readable knowledge pack name." },
                    "slug": { "type": ["string", "null"], "description": "Optional stable pack slug. Omit to derive one from the name." },
                    "description": { "type": ["string", "null"], "description": "Optional pack description." }
                },
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
        ToolSpec {
            name: "update_knowledge_pack".to_string(),
            description: "Update a user-managed Library knowledge pack's name, slug, description, or status.".to_string(),
            parameters: json!({
                "type": "object",
                "required": ["pack"],
                "properties": {
                    "pack": pack_id_schema(),
                    "name": { "type": ["string", "null"], "description": "Optional replacement name." },
                    "slug": { "type": ["string", "null"], "description": "Optional replacement slug." },
                    "description": { "type": ["string", "null"], "description": "Optional replacement description. Use null to clear." },
                    "status": { "type": ["string", "null"], "enum": ["active", "archived", null], "description": "Optional replacement status." }
                },
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
