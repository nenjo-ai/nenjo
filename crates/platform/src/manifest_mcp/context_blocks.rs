use nenjo::{ToolCategory, ToolSpec};

fn context_block_ref_schema() -> serde_json::Value {
    slug_schema(
        "Existing context block slug. Use `slug` from list_context_blocks or get_context_block, not the path-like selector.",
    )
}

fn slug_schema(description: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "string",
        "minLength": 1,
        "maxLength": 255,
        "pattern": "^[a-z0-9](?:[a-z0-9_-]{0,253}[a-z0-9])?$",
        "description": description
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
            description: "Create or update one context block atomically by required stable slug. If the slug does not exist, `name` and `template` are required. Omitted fields are unchanged; set description to null to clear it. Returns the same canonical `context_block: ContextBlockDocument` as get_context_block, not a patch echo."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["slug"],
                "properties": {
                    "slug": slug_schema("Required stable context block slug. Configure never renames this slug."),
                    "name": {
                        "type": "string",
                        "minLength": 1,
                        "description": "Context block runtime/display name. Required when the slug does not exist; omit on update to leave unchanged."
                    },
                    "path": {
                        "type": "string",
                        "description": "Folder path for this context block. Omit to leave unchanged."
                    },
                    "description": {
                        "type": ["string", "null"],
                        "description": "Human-readable description. Omit to leave unchanged; set null to clear."
                    },
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
        assert_eq!(
            configure_context_block.parameters["required"],
            serde_json::json!(["slug"])
        );
        assert!(
            configure_context_block.parameters["properties"]
                .get("metadata")
                .is_none()
        );
    }
}
