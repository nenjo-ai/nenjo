use nenjo::{ToolCategory, ToolSpec};

fn context_block_ref_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "string",
        "description": "Existing context block slug. Use `slug` from list_context_blocks or get_context_block, not the path-like selector. For configure_context_block, omit `context_block` to create a new context block."
    })
}

fn configure_metadata_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "description": "Context block metadata patch. Required on create because metadata.name is required when context_block is omitted. On update, omitted fields are unchanged.",
        "properties": {
            "name": {
                "type": "string",
                "description": "Context block runtime/display name. Required when creating a new context block."
            },
            "path": {
                "type": "string",
                "description": "Folder path for this context block. Omit to leave unchanged."
            },
            "description": {
                "type": ["string", "null"],
                "description": "Human-readable description. Omit to leave unchanged; set null to clear."
            }
        },
        "additionalProperties": false
    })
}

/// Return manifest MCP tool definitions for context block resources.
pub fn context_block_tools() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "list_context_blocks".to_string(),
            description: "List visible context blocks as template-free summaries. Use `slug` for context block tool calls; use dotted `selector` when constructing prompt references. This does not include template; call get_context_block for the full context block document."
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
            description: "Get one context block's full ContextBlockDocument by slug, including template, selector, name, path, and description."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["context_block"],
                "properties": {
                    "context_block": context_block_ref_schema()
                },
                "additionalProperties": false
            }),
            category: ToolCategory::Read,
        },
        ToolSpec {
            name: "configure_context_block".to_string(),
            description: "Create or update one context block in a single backend-owned sequence. Omit `context_block` to create; include `context_block` to update by slug. On create, metadata.name and template are required. Omitted fields are unchanged on update. Returns `context_block: ContextBlockDocument`."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "context_block": context_block_ref_schema(),
                    "metadata": configure_metadata_schema(),
                    "template": {
                        "type": "string",
                        "description": "MiniJinja template content for this context block. Omit to leave unchanged on update."
                    }
                },
                "additionalProperties": false
            }),
            category: ToolCategory::Write,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configure_context_block_exposes_template() {
        let tools = context_block_tools();
        let configure_context_block = tools
            .iter()
            .find(|tool| tool.name == "configure_context_block")
            .expect("configure_context_block tool should exist");

        assert_eq!(
            configure_context_block.parameters["properties"]["template"]["description"],
            serde_json::json!(
                "MiniJinja template content for this context block. Omit to leave unchanged on update."
            )
        );
        assert!(configure_context_block.description.contains("template"));
    }
}
