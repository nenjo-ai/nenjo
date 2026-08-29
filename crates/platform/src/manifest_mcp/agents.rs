use nenjo::{ToolCategory, ToolSpec};

fn agent_ref_schema() -> serde_json::Value {
    slug_schema("Existing agent slug. Use `slug` from list_agents or get_agent.")
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

fn slug_list_schema(description: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "array",
        "description": description,
        "items": slug_schema("Assigned resource slug."),
        "uniqueItems": true
    })
}

fn prompt_config_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "description": "Partial prompt configuration patch for the target agent. Omit any field you do not want to change.",
        "properties": {
            "system_prompt": {
                "type": "string",
                "description": "Highest-level instruction for the agent. Defines the agent's role, boundaries, and non-negotiable behavior. Omit to leave unchanged."
            },
            "developer_prompt": {
                "type": "string",
                "description": "Secondary guidance for the agent. Used for implementation detail, workflow rules, and contextual guidance beneath the system prompt. Omit to leave unchanged."
            },
            "templates": {
                "type": "object",
                "description": "Agent template slot patch. Only provided keys are updated.",
                "properties": {
                    "task": {
                        "type": "string",
                        "description": "Template used when the agent executes a normal task. Omit to leave unchanged."
                    },
                    "chat": {
                        "type": "string",
                        "description": "Template used when the agent responds in chat. Omit to leave unchanged."
                    },
                    "gate": {
                        "type": "string",
                        "description": "Template used when the agent evaluates a gate. Omit to leave unchanged."
                    }
                },
                "additionalProperties": true
            },
            "memory_profile": {
                "type": "object",
                "description": "Partial memory extraction and retrieval preference patch for the target agent.",
                "properties": {
                    "core_focus": {
                        "type": "array",
                        "description": "Cross-project topics this agent wants remembered as durable core knowledge. Provide the full replacement list for this field.",
                        "items": { "type": "string" }
                    },
                    "project_focus": {
                        "type": "array",
                        "description": "Project-specific topics this agent wants remembered within the active project context. Provide the full replacement list for this field.",
                        "items": { "type": "string" }
                    },
                    "shared_focus": {
                        "type": "array",
                        "description": "Topics this agent should prefer to store into shared memory for reuse by other agents. Provide the full replacement list for this field.",
                        "items": { "type": "string" }
                    }
                },
                "additionalProperties": false
            }
        },
        "additionalProperties": false
    })
}

/// Return manifest MCP tool definitions for agent resources.
pub fn agent_tools() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "list_agents".to_string(),
            description: "List visible agents as prompt-free summaries. Use a returned `slug` as the `agent` value in get_agent or configure_agent. This does not include prompt_config; call get_agent for the full agent document."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
            category: ToolCategory::Read,
        },
        ToolSpec {
            name: "get_agent".to_string(),
            description: "Get one agent's full AgentDocument by slug, including prompt_config, assignments, platform_scopes, and prompt lock state."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["agent"],
                "properties": {
                    "agent": agent_ref_schema()
                },
                "additionalProperties": false
            }),
            category: ToolCategory::Read,
        },
        ToolSpec {
            name: "configure_agent".to_string(),
            description: "Create or update one agent atomically by required stable slug. If the slug does not exist, `name` is required. Omitted fields are unchanged. Set nullable fields to null to clear them. prompt_config is a partial merge patch. Assignment arrays are full replacements when present; pass an empty array to clear that assignment type. Returns the same canonical `agent: AgentDocument` as get_agent, not a patch echo."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["slug"],
                "properties": {
                    "slug": slug_schema("Required stable agent slug. Configure never renames this slug."),
                    "name": {
                        "type": "string",
                        "minLength": 1,
                        "description": "Agent runtime/display name. Required when the slug does not exist; omit on update to leave unchanged."
                    },
                    "description": {
                        "type": ["string", "null"],
                        "description": "Human-readable description. Omit to leave unchanged; set null to clear."
                    },
                    "color": {
                        "type": ["string", "null"],
                        "description": "Dashboard color. Omit to leave unchanged; set null to clear."
                    },
                    "model": {
                        "type": ["string", "null"],
                        "description": "Model slug. Omit to leave unchanged; set null to clear the direct model assignment."
                    },
                    "prompt_config": prompt_config_schema(),
                    "abilities": slug_list_schema("Full replacement list of ability slugs assigned to this agent. Omit to leave unchanged."),
                    "domains": slug_list_schema("Full replacement list of domain slugs assigned to this agent. Omit to leave unchanged."),
                    "mcp_servers": slug_list_schema("Full replacement list of MCP server slugs assigned to this agent. Omit to leave unchanged.")
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
    fn configure_agent_nullable_fields_clear_values() {
        let tools = agent_tools();
        let configure_agent = tools
            .iter()
            .find(|tool| tool.name == "configure_agent")
            .expect("configure_agent tool should exist");
        let properties = &configure_agent.parameters["properties"];

        assert_eq!(
            properties["description"]["type"],
            serde_json::json!(["string", "null"])
        );
        assert!(
            properties["description"]["description"]
                .as_str()
                .unwrap_or_default()
                .contains("clear")
        );
    }

    #[test]
    fn configure_agent_exposes_mcp_server_assignments() {
        let tools = agent_tools();
        let configure_agent = tools
            .iter()
            .find(|tool| tool.name == "configure_agent")
            .expect("configure_agent tool should exist");

        assert_eq!(
            configure_agent.parameters["properties"]["mcp_servers"]["description"],
            serde_json::json!(
                "Full replacement list of MCP server slugs assigned to this agent. Omit to leave unchanged."
            )
        );
        assert!(
            configure_agent
                .description
                .contains("Assignment arrays are full replacements")
        );
        assert_eq!(
            configure_agent.parameters["required"],
            serde_json::json!(["slug"])
        );
    }
}
