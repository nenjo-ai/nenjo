use nenjo::{ToolCategory, ToolSpec};
use serde_json::json;

fn doc_slug_schema() -> serde_json::Value {
    json!({
        "type": "string",
        "description": "Stable library knowledge document slug. Use knowledge_doc.slug returned by create_knowledge_doc or document.slug returned by read/search metadata; do not invent slugs from titles unless the document was just returned with that slug."
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
            description: "Create a new org-level library knowledge document. Provide filename and optional folder path; the platform derives the document slug from path plus filename and returns it as knowledge_doc.slug. The response also includes edges when related is provided. If creating multiple documents that relate to each other, create all documents first, collect their returned knowledge_doc.slug values, then call update_knowledge_doc once per source document with the full related list.".to_string(),
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
                    "related": related_schema("Optional full outbound relationship list for this new document. Targets must already exist. If the targets are being created in the same workflow, omit related here and set it later with update_knowledge_doc after collecting the target knowledge_doc.slug values.")
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
            description: "Update an existing library knowledge document. Requires slug, the document slug returned by create_knowledge_doc or discovered from read/search metadata. Providing related performs one canonical full replacement of the document's outbound edge list and returns the stored edge records.".to_string(),
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
                    "related": related_schema("Optional full outbound relationship replacement list. Providing related replaces every outbound edge from this document in one backend operation; omit related to leave existing edges unchanged. Use an empty array to remove all outbound edges.")
                },
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
    ]
}

fn related_schema(description: &str) -> serde_json::Value {
    json!({
        "type": "array",
        "description": description,
        "items": {
            "type": "object",
            "required": ["target_doc", "type"],
            "properties": {
                "target_doc": doc_slug_schema(),
                "type": {
                    "type": "string",
                    "enum": [
                        "references",
                        "depends_on",
                        "defines",
                        "part_of",
                        "extends",
                        "related_to",
                        "governs",
                        "classifies"
                    ],
                    "description": "Relationship type for this outbound edge."
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
