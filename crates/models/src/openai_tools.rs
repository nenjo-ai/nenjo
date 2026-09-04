//! Shared OpenAI-compatible function-tool request representation.
//!
//! OpenAI, OpenRouter, generic OpenAI-compatible endpoints, and Ollama all
//! accept this wire shape. Providers retain their own conversion wrappers when
//! their empty-tool or name-sanitization behavior differs.

use crate::ToolSpec;
use serde::Serialize;
use std::collections::HashMap;

#[derive(Debug, Serialize)]
pub(crate) struct ProviderToolSpec {
    #[serde(rename = "type")]
    pub(crate) kind: String,
    pub(crate) function: NativeToolFunctionSpec,
}

#[derive(Debug, Serialize)]
pub(crate) struct NativeToolFunctionSpec {
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) parameters: serde_json::Value,
}

pub(crate) fn convert_tools(
    tools: Option<&[ToolSpec]>,
    sanitize_name: impl Fn(&str) -> String,
) -> Option<Vec<ProviderToolSpec>> {
    tools.map(|items| {
        items
            .iter()
            .map(|tool| ProviderToolSpec {
                kind: "function".to_string(),
                function: NativeToolFunctionSpec {
                    name: sanitize_name(&tool.name),
                    description: tool.description.clone(),
                    parameters: tool.parameters.clone(),
                },
            })
            .collect()
    })
}

/// Convert tools while rejecting names that become ambiguous in the provider
/// dialect. Input order is preserved exactly.
pub(crate) fn convert_tools_checked(
    tools: Option<&[ToolSpec]>,
    sanitize_name: impl Fn(&str) -> String,
) -> anyhow::Result<Option<Vec<ProviderToolSpec>>> {
    let Some(tools) = tools else {
        return Ok(None);
    };
    let mut canonical_by_wire_name = HashMap::with_capacity(tools.len());
    let mut converted = Vec::with_capacity(tools.len());
    for tool in tools {
        let wire_name = sanitize_name(&tool.name);
        if let Some(existing) = canonical_by_wire_name.insert(wire_name.clone(), &tool.name) {
            anyhow::bail!(
                "tool names '{existing}' and '{}' both serialize as '{wire_name}'",
                tool.name
            );
        }
        converted.push(ProviderToolSpec {
            kind: "function".to_string(),
            function: NativeToolFunctionSpec {
                name: wire_name,
                description: tool.description.clone(),
                parameters: tool.parameters.clone(),
            },
        });
    }
    Ok(Some(converted))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_openai_function_tool_shape() {
        let tools = vec![ToolSpec {
            name: "app.nenjo.platform/tasks".into(),
            description: "Manage tasks".into(),
            parameters: serde_json::json!({"type": "object"}),
            category: Default::default(),
        }];

        let converted = convert_tools(Some(&tools), crate::sanitize_tool_name).unwrap();

        assert_eq!(
            serde_json::to_value(converted).unwrap(),
            serde_json::json!([{
                "type": "function",
                "function": {
                    "name": "app_nenjo_platform_tasks",
                    "description": "Manage tasks",
                    "parameters": {"type": "object"}
                }
            }])
        );
    }
}
