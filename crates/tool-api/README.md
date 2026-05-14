# nenjo-tool-api

Shared tool contracts for Nenjo agents, model providers, and runtimes.

This crate is intentionally small and independent from the other Nenjo workspace crates. It defines the stable types needed to describe tools, receive model tool calls, execute tools, and return tool results.

## Contents

- `Tool` — async trait implemented by concrete runtime tools.
- `ToolSpec` — JSON-schema-backed tool registration metadata sent to model providers.
- `ToolCategory` — read/write side-effect classification used for guidance and filtering.
- `ToolCall` — model-requested tool invocation.
- `ToolResult` and `ToolResultMessage` — execution results and provider feedback payloads.
- `ToolAutonomy` and `ToolSecurity` — SDK-level policy inputs used when constructing tools.
- `sanitize_tool_name` and `sanitize_tool_name_lenient` — provider-safe tool name helpers.

## Usage

```rust
use async_trait::async_trait;
use serde_json::json;
use nenjo_tool_api::{Tool, ToolCategory, ToolResult};

struct EchoTool;

#[async_trait]
impl Tool for EchoTool {
    fn name(&self) -> &str {
        "echo"
    }

    fn description(&self) -> &str {
        "Echoes a message back to the caller."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "message": { "type": "string" }
            },
            "required": ["message"]
        })
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Read
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        Ok(ToolResult {
            success: true,
            output: args["message"].as_str().unwrap_or_default().to_string(),
            error: None,
        })
    }
}
```

## Dependency Boundary

`nenjo-tool-api` must not depend on other Nenjo workspace crates. Workspace crates may depend on it or re-export its types for compatibility, but the ownership of the tool contracts stays here.

## License

Licensed under the Apache License, Version 2.0. See [LICENSE](../../LICENSE) for details.
