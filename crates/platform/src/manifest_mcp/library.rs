use nenjo::{ToolCategory, ToolSpec};
use serde_json::json;

fn doc_slug_schema() -> serde_json::Value {
    json!({
        "type": "string",
        "description": "The stable slug of the target library knowledge document. Use knowledge_doc.slug returned by create_knowledge_doc or document.slug returned by read/search metadata."
    })
}

fn pack_slug_schema() -> serde_json::Value {
    json!({
        "type": "string",
        "description": "The stable slug of the org-level library knowledge pack, such as user-rust-skills. Do not use selector syntax such as lib:user-rust-skills."
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
            description: "Update a user-managed Library knowledge pack's name, slug, or description.".to_string(),
            parameters: json!({
                "type": "object",
                "required": ["pack"],
                "properties": {
                    "pack": pack_slug_schema(),
                    "name": { "type": ["string", "null"], "description": "Optional replacement name." },
                    "slug": { "type": ["string", "null"], "description": "Optional replacement slug." },
                    "description": { "type": ["string", "null"], "description": "Optional replacement description. Use null to clear." }
                },
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
        ToolSpec {
            name: "create_knowledge_doc".to_string(),
            description: "Create a new org-level library knowledge document. Provide filename and optional folder path; the platform derives the document slug from path plus filename and returns it as knowledge_doc.slug. If creating multiple related documents, create them first, collect each returned knowledge_doc.slug, then call update_knowledge_doc to assign related edges.".to_string(),
            parameters: json!({
                "type": "object",
                "required": ["pack", "filename", "content"],
                "properties": {
                    "pack": pack_slug_schema(),
                    "filename": { "type": "string", "description": "Filesystem-safe filename used for storage, such as ownership-lifetimes.md." },
                    "content": { "type": "string", "description": "Full text content for the new library knowledge document." },
                    "content_type": { "type": ["string", "null"], "description": "Optional MIME type such as text/markdown or application/json." },
                    "path": { "type": ["string", "null"], "description": "Optional library-relative folder path, such as rust/ownership. This is a folder only; do not include the filename." },
                    "title": { "type": ["string", "null"], "description": "Optional human-readable display title, such as Ownership & Lifetimes." },
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
            description: "Delete an existing library knowledge document. Requires slug, the document slug returned by create_knowledge_doc or discovered from read/search metadata.".to_string(),
            parameters: json!({
                "type": "object",
                "required": ["pack", "slug"],
                "properties": {
                    "pack": pack_slug_schema(),
                    "slug": doc_slug_schema()
                },
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
        ToolSpec {
            name: "update_knowledge_doc".to_string(),
            description: "Update an existing library knowledge document. Requires slug, the document slug returned by create_knowledge_doc or discovered from read/search metadata. Providing related replaces the document's full outbound relationship list; use this after creating documents and collecting their returned slugs.".to_string(),
            parameters: json!({
                "type": "object",
                "required": ["pack", "slug"],
                "properties": {
                    "pack": pack_slug_schema(),
                    "slug": doc_slug_schema(),
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
        "description": "Optional full outbound relationship list. On update, providing this replaces existing outbound edges for the document. Each target_doc must be the stable document.slug from create_knowledge_doc or search/read metadata (preferred), or a resolvable selector/path; create all target documents before assigning relations.",
        "items": {
            "type": "object",
            "required": ["target_doc", "type"],
            "properties": {
                "target_doc": doc_slug_schema(),
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
