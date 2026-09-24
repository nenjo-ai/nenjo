//! The `script` tool: lets an agent run a short JavaScript program that
//! dispatches its available tools directly from the script.
//!
//! Every tool the agent could call directly is exposed as a method on a
//! context namespace (`ctx.mcp.<tool>()` in phase 1, `ctx.runtime`/`ctx.harness`
//! later). Dispatches re-enter the normal tool pipeline, so scripts gain
//! control flow — fan-out, branching, aggregation — never privileges.
//!
//! See BOO-61.

pub mod engine;

use std::sync::Arc;

use async_trait::async_trait;
use nenjo_tool_api::{Tool, ToolCategory, ToolOrigin, ToolResult};
use serde_json::json;

use self::engine::{ScriptLimits, ScriptNamespaces, ScriptOutcome};

pub const SCRIPT_TOOL_NAME: &str = "script";

/// Tool implementation wrapping the QuickJS engine.
pub struct ScriptTool {
    /// Tools dispatchable from the script's `ctx.mcp` namespace.
    mcp_tools: Vec<Arc<dyn Tool>>,
    limits: ScriptLimits,
    description: String,
}

impl ScriptTool {
    /// Build a script tool exposing the agent's MCP tools under `ctx.mcp`.
    pub fn mcp(mcp_tools: Vec<Arc<dyn Tool>>) -> Self {
        let tool_names = mcp_tools
            .iter()
            .map(|tool| tool.name())
            .collect::<Vec<_>>()
            .join(", ");
        let description = format!(
            "Run a short JavaScript (QuickJS) program to orchestrate multiple tool calls in one \
             step. All MCP tools are available as async methods on the `ctx.mcp` namespace: \
             `await ctx.mcp.<toolName>({{...args}})` returns `{{ok, content, error}}`; \
             `ctx.mcp.list()` returns the callable tools with their schemas; `ctx.log(...)` \
             streams progress notes. Fan out with `Promise.all`, branch on results, and `return` \
             a JSON-serializable summary object — the return value is the tool result. Available \
             MCP tools: {tool_names}. The script cannot access the filesystem, network, or \
             environment; each dispatched call goes through the same permission checks as direct \
             tool calls."
        );
        Self {
            mcp_tools,
            limits: ScriptLimits::default(),
            description,
        }
    }

    fn namespaces(&self) -> ScriptNamespaces {
        ScriptNamespaces {
            mcp: self.mcp_tools.clone(),
        }
    }
}

#[async_trait]
impl Tool for ScriptTool {
    fn name(&self) -> &str {
        SCRIPT_TOOL_NAME
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "script": {
                    "type": "string",
                    "description": "JavaScript source. Runs inside an async function; use `await`, top-level `return`, `ctx.mcp.*` tool methods, and `ctx.log`."
                },
                "timeout_ms": {
                    "type": "integer",
                    "description": "Optional wall-clock budget in milliseconds. Defaults to 30000, capped at 120000."
                }
            },
            "required": ["script"]
        })
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::ReadWrite
    }

    fn origin(&self) -> ToolOrigin {
        ToolOrigin::Host
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let script = args.get("script").and_then(|value| value.as_str());
        let Some(script) = script else {
            return Ok(ToolResult::failure("missing required argument `script`"));
        };
        if script.len() > 512 * 1024 {
            return Ok(ToolResult::failure("script exceeds 512 KiB size limit"));
        }
        let timeout = args
            .get("timeout_ms")
            .and_then(|value| value.as_u64())
            .map(std::time::Duration::from_millis)
            .unwrap_or(self.limits.default_timeout);

        let outcome = engine::run(script, self.namespaces(), &self.limits, timeout).await?;
        Ok(outcome_to_result(&outcome))
    }
}

fn outcome_to_result(outcome: &ScriptOutcome) -> ToolResult {
    if let Some(error) = &outcome.error {
        let mut text = error.clone();
        if !outcome.logs.is_empty() {
            text.push_str("\n\nscript log:\n");
            text.push_str(&outcome.logs.join("\n"));
        }
        return ToolResult::failure(text);
    }

    // Machine-readable envelope so the model can parse the return value even
    // when log lines are present.
    let payload = json!({
        "result": outcome.value.clone().unwrap_or(serde_json::Value::Null),
        "log": outcome.logs,
    });
    ToolResult::success(serde_json::to_string(&payload).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nenjo_tool_api::ToolCategory;
    use serde_json::json;
    use std::time::Duration;

    /// Deterministic stand-in for an `ExternalMcpTool`.
    struct FakeMcpTool {
        name: &'static str,
        behavior: FakeBehavior,
    }

    enum FakeBehavior {
        /// Echoes the call arguments back as JSON.
        Echo,
        /// Always returns a denial.
        Deny(&'static str),
    }

    impl FakeMcpTool {
        fn echo(name: &'static str) -> Self {
            Self {
                name,
                behavior: FakeBehavior::Echo,
            }
        }

        fn deny(name: &'static str, reason: &'static str) -> Self {
            Self {
                name,
                behavior: FakeBehavior::Deny(reason),
            }
        }

        fn arc(self) -> Arc<dyn Tool> {
            Arc::new(self)
        }
    }

    #[async_trait]
    impl Tool for FakeMcpTool {
        fn name(&self) -> &str {
            self.name
        }

        fn description(&self) -> &str {
            "fake mcp tool for script tests"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            json!({"type": "object"})
        }

        fn category(&self) -> ToolCategory {
            ToolCategory::Read
        }

        fn origin(&self) -> ToolOrigin {
            ToolOrigin::Mcp
        }

        async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
            match &self.behavior {
                FakeBehavior::Echo => Ok(ToolResult::success(args.to_string())),
                FakeBehavior::Deny(reason) => Ok(ToolResult::failure(*reason)),
            }
        }
    }

    fn script_tool(tools: Vec<Arc<dyn Tool>>) -> ScriptTool {
        ScriptTool::mcp(tools)
    }

    async fn run_script(tool: &ScriptTool, script: &str) -> ToolResult {
        tool.execute(json!({"script": script, "timeout_ms": 5_000}))
            .await
            .expect("execute should not error at the transport level")
    }

    #[tokio::test]
    async fn script_dispatches_mcp_tools_and_returns_value() {
        let tool = script_tool(vec![
            FakeMcpTool::echo("mcp_test__get_issue").arc(),
            FakeMcpTool::echo("mcp_test__list_issues").arc(),
        ]);
        let result = run_script(
            &tool,
            r#"
                const direct = await ctx.mcp.mcp_test__get_issue({ id: 42 });
                const [a, b] = await Promise.all([
                    ctx.mcp.mcp_test__get_issue({ id: 1 }),
                    ctx.mcp.mcp_test__list_issues({ state: "open" }),
                ]);
                ctx.log("fetched", direct.ok ? "ok" : "failed");
                return { ids: [a.content, b.content], direct: direct.ok };
            "#,
        )
        .await;
        assert!(result.success, "script failed: {:?}", result.error);
        let value: serde_json::Value = serde_json::from_str(&result.output.text_content())
            .expect("JSON envelope");
        assert_eq!(value["result"]["direct"], json!(true));
        assert_eq!(
            value["result"]["ids"],
            json!([r#"{"id":1}"#, r#"{"state":"open"}"#])
        );
        assert_eq!(value["log"], json!(["fetched ok"]));
    }

    #[tokio::test]
    async fn script_surfaces_denials_like_direct_calls() {
        let tool = script_tool(vec![FakeMcpTool::deny("mcp_test__write_file", "denied by policy").arc()]);
        let result = run_script(
            &tool,
            "return await ctx.mcp.mcp_test__write_file({ path: 'x' });",
        )
        .await;
        assert!(result.success, "script itself succeeds; denial is data");
        let envelope: serde_json::Value =
            serde_json::from_str(&result.output.text_content()).expect("JSON envelope");
        assert_eq!(envelope["result"]["ok"], json!(false));
        assert_eq!(envelope["result"]["error"], json!("denied by policy"));
    }

    #[tokio::test]
    async fn script_dispatch_of_unknown_tool_is_a_data_error() {
        let tool = script_tool(vec![FakeMcpTool::echo("mcp_test__known").arc()]);
        let result = run_script(
            &tool,
            "return await ctx.mcp.mcp_test__does_not_exist({});",
        )
        .await;
        // The shim only defines methods for granted tools, so a typo'd tool is
        // an ordinary JS TypeError the model can read and fix.
        assert!(!result.success);
        let error = result.error.as_deref().expect("error set");
        assert!(error.contains("not a function"), "unexpected error: {error}");
    }

    #[tokio::test]
    async fn busy_loop_is_killed_by_timeout() {
        let tool = script_tool(vec![FakeMcpTool::echo("mcp_test__echo").arc()]);
        let started = std::time::Instant::now();
        let result = tool
            .execute(json!({"script": "while (true) {}", "timeout_ms": 300}))
            .await
            .expect("execute should not error at the transport level");
        assert!(!result.success, "busy loop must not succeed");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "timeout must actually kill the interpreter"
        );
        assert!(
            result.error.as_deref().unwrap_or_default().contains("timeout"),
            "unexpected error: {:?}",
            result.error
        );
    }

    #[tokio::test]
    async fn list_and_log_are_exposed() {
        let tool = script_tool(vec![
            FakeMcpTool::echo("mcp_test__a").arc(),
            FakeMcpTool::echo("mcp_test__b").arc(),
        ]);
        let result = run_script(
            &tool,
            r#"
                const names = (await ctx.mcp.list()).map(t => t.name);
                return { names, hasLog: typeof ctx.log === "function" };
            "#,
        )
        .await;
        assert!(result.success, "script failed: {:?}", result.error);
        let envelope: serde_json::Value =
            serde_json::from_str(&result.output.text_content()).expect("JSON envelope");
        assert_eq!(
            envelope["result"]["names"],
            json!(["mcp_test__a", "mcp_test__b"])
        );
        assert_eq!(envelope["result"]["hasLog"], json!(true));
    }

    #[tokio::test]
    async fn oversized_return_value_is_rejected() {
        let tool = script_tool(vec![FakeMcpTool::echo("mcp_test__echo").arc()]);
        let mut limits_tool = script_tool(vec![FakeMcpTool::echo("mcp_test__echo").arc()]);
        limits_tool.limits.max_output_bytes = 1_000;
        let _ = tool; // keep the default-limit instance referenced for symmetry
        let result = run_script(
            &limits_tool,
            "return { blob: 'x'.repeat(100_000) };",
        )
        .await;
        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .expect("error set")
                .contains("exceeds output cap"),
            "unexpected error: {:?}",
            result.error
        );
    }

    #[tokio::test]
    async fn missing_script_argument_is_rejected() {
        let tool = script_tool(vec![FakeMcpTool::echo("mcp_test__echo").arc()]);
        let result = tool
            .execute(json!({}))
            .await
            .expect("execute should not error at the transport level");
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap_or_default().contains("script"));
    }

    #[test]
    fn script_tool_is_absent_without_mcp_tools_in_description_namespace() {
        // The factory only registers ScriptTool when MCP tools exist; the tool
        // itself must still advertise an empty catalog rather than crash.
        let tool = script_tool(vec![]);
        let spec = tool.spec();
        assert_eq!(spec.name, SCRIPT_TOOL_NAME);
        assert!(spec.parameters["properties"]["script"].is_object());
    }
}
