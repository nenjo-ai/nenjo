# nenjo-tools

Tool trait, types, and built-in tool implementations for the Nenjo agent platform.

## Overview

This crate defines the `Tool` trait that all agent tools implement, along with built-in implementations for common operations.

## Built-in tools

- **Shell** — execute commands with timeout and output capture
- **File** — read, write, edit, and search files
- **Git** — repository operations (status, diff, commit, branch)
- **Search** — glob and regex search across codebases
- **Web** — HTTP requests and web content fetching
- **Memory** — persistent agent memory (store, recall, forget)

## Custom tools

Implement the `Tool` trait to create your own:

```rust,ignore
use nenjo_tools::{Tool, ToolResult, ToolCategory};

struct MyTool;

#[async_trait::async_trait]
impl Tool for MyTool {
    fn name(&self) -> &str { "my_tool" }
    fn description(&self) -> &str { "Does something useful" }
    fn category(&self) -> ToolCategory { ToolCategory::ReadOnly }
    fn parameters_schema(&self) -> serde_json::Value { /* ... */ }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        Ok(ToolResult { success: true, output: "done".into(), error: None })
    }
}
```

## License

Licensed under the Apache License, Version 2.0. See [LICENSE](../../LICENSE) for details.
