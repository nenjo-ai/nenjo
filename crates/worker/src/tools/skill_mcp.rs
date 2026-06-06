use std::sync::Arc;

use async_trait::async_trait;
use nenjo::ToolOrigin;
use nenjo::skills::{CALL_SKILL_MCP_TOOL_NAME, SkillRuntimeState};
use serde_json::{Value, json};

use crate::external_mcp::ExternalMcpPool;
use crate::tools::{Tool, ToolCategory, ToolResult};

pub struct SkillMcpTool {
    external_mcp: Arc<ExternalMcpPool>,
    skill_runtime: Arc<SkillRuntimeState>,
}

impl SkillMcpTool {
    pub fn new(external_mcp: Arc<ExternalMcpPool>, skill_runtime: Arc<SkillRuntimeState>) -> Self {
        Self {
            external_mcp,
            skill_runtime,
        }
    }
}

#[async_trait]
impl Tool for SkillMcpTool {
    fn name(&self) -> &str {
        CALL_SKILL_MCP_TOOL_NAME
    }

    fn description(&self) -> &str {
        "Call an MCP tool made available by an activated skill. Use this only after use_skill has activated a skill that declares MCP servers."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "server": {
                    "type": "string",
                    "description": "Optional active MCP server slug or name. Required when more than one active skill MCP server exposes the same tool."
                },
                "tool": {
                    "type": "string",
                    "description": "MCP tool name to call"
                },
                "arguments": {
                    "type": "object",
                    "description": "JSON arguments for the MCP tool",
                    "additionalProperties": true
                }
            },
            "required": ["tool"],
            "additionalProperties": false
        })
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::ReadWrite
    }

    fn origin(&self) -> ToolOrigin {
        ToolOrigin::Mcp
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let tool = args
            .get("tool")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("Missing 'tool' parameter"))?;
        let server = args.get("server").and_then(Value::as_str);
        let arguments = args.get("arguments").cloned().unwrap_or_else(|| json!({}));
        if !arguments.is_object() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("'arguments' must be an object".to_string()),
            });
        }

        let active_servers = self.skill_runtime.active_mcp_servers();
        match self
            .external_mcp
            .call_skill_mcp_tool(&active_servers, server, tool, arguments)
            .await
        {
            Ok(output) => Ok(ToolResult {
                success: true,
                output,
                error: None,
            }),
            Err(error) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(error.to_string()),
            }),
        }
    }
}
