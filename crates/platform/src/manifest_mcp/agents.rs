use nenjo::{ToolCategory, ToolSpec};

fn agent_ref_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "string",
        "description": "Existing agent slug. Use `slug` from list_agents or get_agent. For configure_agent, omit `agent` to create a new agent."
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
                    },
                    "heartbeat": {
                        "type": "string",
                        "description": "Template used when the agent is invoked by a heartbeat schedule. Omit to leave unchanged."
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

fn configure_metadata_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "description": "Agent metadata patch. Required on create because metadata.name is required when agent is omitted. On update, omitted fields are unchanged. Local manifest backend stores color as optional; platform backend resets color to its default when color is null because platform color is non-null.",
        "properties": {
            "name": {
                "type": "string",
                "description": "Agent runtime/display name. Required when creating a new agent."
            },
            "description": {
                "type": ["string", "null"],
                "description": "Human-readable description. Omit to leave unchanged; set null to clear on both platform and local manifest backends."
            },
            "color": {
                "type": ["string", "null"],
                "description": "Hex dashboard color. Omit to leave unchanged. Set null to clear local manifest color; on the platform backend, null resets to the default dashboard color because platform color is non-null."
            },
            "model": {
                "type": ["string", "null"],
                "description": "Model slug. Omit to leave unchanged; set null to clear the direct model assignment."
            }
        },
        "additionalProperties": false
    })
}

fn configure_assignments_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "description": "Full replacement assignment lists. Omit a field to leave that assignment type unchanged; pass an empty array to clear it.",
        "properties": {
            "abilities": {
                "type": "array",
                "description": "Full replacement list of ability names/slugs assigned to this agent.",
                "items": { "type": "string" }
            },
            "domains": {
                "type": "array",
                "description": "Full replacement list of domain slugs assigned to this agent.",
                "items": { "type": "string" }
            },
            "mcp_servers": {
                "type": "array",
                "description": "Full replacement list of MCP server slugs assigned to this agent.",
                "items": { "type": "string" }
            }
        },
        "additionalProperties": false
    })
}

fn configure_heartbeat_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "description": "Heartbeat schedule patch. interval is required when setting instructions on an agent without an existing heartbeat.",
        "properties": {
            "interval": {
                "type": "string",
                "description": "Cron expression or interval string for the heartbeat schedule."
            },
            "metadata": {
                "type": "object",
                "description": "Optional heartbeat metadata such as timezone.",
                "additionalProperties": true
            },
            "instructions": {
                "type": "string",
                "description": "Heartbeat instructions for scheduled agent runs."
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
            description: "Get one agent's full AgentDocument by slug, including prompt_config, assignments, platform_scopes, prompt lock state, and heartbeat."
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
            description: "Create or update one agent in a single backend-owned sequence. Omit `agent` to create; include `agent` to update by slug. On create, metadata.name is required. Omitted fields are unchanged on update. prompt_config is a partial merge patch. assignment arrays are full replacements when present; pass an empty array to clear that assignment type. Returns `agent: AgentDocument`."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "agent": agent_ref_schema(),
                    "metadata": configure_metadata_schema(),
                    "prompt_config": prompt_config_schema(),
                    "assignments": configure_assignments_schema(),
                    "heartbeat": configure_heartbeat_schema()
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
        let metadata = &configure_agent.parameters["properties"]["metadata"]["properties"];

        assert_eq!(
            metadata["description"]["type"],
            serde_json::json!(["string", "null"])
        );
        assert!(
            metadata["description"]["description"]
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
            configure_agent.parameters["properties"]["assignments"]["properties"]["mcp_servers"]["description"],
            serde_json::json!("Full replacement list of MCP server slugs assigned to this agent.")
        );
        assert!(
            configure_agent
                .description
                .contains("assignment arrays are full replacements")
        );
    }
}
