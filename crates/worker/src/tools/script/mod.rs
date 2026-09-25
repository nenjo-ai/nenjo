//! The `script` tool: lets an agent run a short JavaScript program that
//! dispatches its available tools directly from the script.
//!
//! Every tool the agent could call directly is exposed as a method on a
//! context namespace: `ctx.mcp.<tool>()` for MCP tools, `ctx.runtime.<tool>()`
//! for host tools, and `ctx.harness` for session/project identity. Dispatches
//! re-enter the normal tool pipeline, so scripts gain control flow — fan-out,
//! branching, aggregation — never privileges.
//!
//! Scripts that outlive `initial_wait` are promoted to an async operation
//! (`AsyncOperationKind::Script`) with inspect/stop/wait controls, mirroring
//! the shell tool's behavior.
//!
//! See BOO-61.

pub mod engine;
pub(crate) mod lifecycle;
pub mod package;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use nenjo::current_async_operation_runtime;
use nenjo::scope_async_operation_runtime;
use nenjo_tool_api::{Tool, ToolCategory, ToolOrigin, ToolResult};
use serde_json::json;
use tokio_util::sync::CancellationToken;

pub use self::engine::ScriptHarnessContext;
use self::engine::{LogBuffer, ScriptLimits, ScriptNamespaces, ScriptOutcome};
use self::lifecycle::{INITIAL_WAIT, promote_to_operation};

pub const SCRIPT_TOOL_NAME: &str = "script";

/// Tool implementation wrapping the QuickJS engine.
pub struct ScriptTool {
    /// Tools dispatchable from `ctx.mcp`.
    mcp_tools: Vec<Arc<dyn Tool>>,
    /// Tools dispatchable from `ctx.runtime`.
    runtime_tools: Vec<Arc<dyn Tool>>,
    /// Identity exposed via `ctx.harness`.
    harness: Option<ScriptHarnessContext>,
    limits: ScriptLimits,
    initial_wait: Duration,
    description: String,
}

impl ScriptTool {
    /// Build a script tool exposing MCP and host tools as context namespaces.
    pub fn new(
        mcp_tools: Vec<Arc<dyn Tool>>,
        runtime_tools: Vec<Arc<dyn Tool>>,
        harness: Option<ScriptHarnessContext>,
    ) -> Self {
        let mut sections = Vec::new();
        if !mcp_tools.is_empty() {
            sections.push(format!(
                "`ctx.mcp.<toolName>({{...args}})` for MCP tools: {}",
                tool_name_list(&mcp_tools)
            ));
        }
        if !runtime_tools.is_empty() {
            sections.push(format!(
                "`ctx.runtime.<toolName>({{...args}})` for host tools: {}",
                tool_name_list(&runtime_tools)
            ));
        }
        if harness.is_some() {
            sections.push(
                "`ctx.harness` for session/project identity (`sessionId`, `project`)".to_string(),
            );
        }
        let dispatch_sections = if sections.is_empty() {
            "none are granted to this agent".to_string()
        } else {
            sections.join("; ")
        };
        let description = format!(
            "Run a short JavaScript (QuickJS) program to orchestrate multiple tool calls in one \
             step. Every namespace method returns `{{ok, content, error}}`; \
             `ctx.<namespace>.list()` returns the callable tools with their schemas; \
             `ctx.log(...)` streams progress notes. Fan out with `Promise.all`, branch on \
             results, and `return` a JSON-serializable summary object — the return value is the \
             tool result. Dispatch surface: {dispatch_sections}. Scripts run for up to 2 minutes; \
             longer scripts keep running in the background and return an operation id usable \
             with the inspect/stop/wait tools. The script cannot access the filesystem, network, \
             or environment; each dispatched call goes through the same permission checks as \
             direct tool calls."
        );
        Self {
            mcp_tools,
            runtime_tools,
            harness,
            limits: ScriptLimits::default(),
            initial_wait: INITIAL_WAIT,
            description,
        }
    }

    /// Override the synchronous phase before promotion (used by tests and
    /// tuning; production default is 2s).
    pub fn with_initial_wait(
        mcp_tools: Vec<Arc<dyn Tool>>,
        runtime_tools: Vec<Arc<dyn Tool>>,
        harness: Option<ScriptHarnessContext>,
        initial_wait: Duration,
    ) -> Self {
        let mut tool = Self::new(mcp_tools, runtime_tools, harness);
        tool.initial_wait = initial_wait;
        tool
    }

    fn namespaces(&self) -> ScriptNamespaces {
        ScriptNamespaces {
            mcp: self.mcp_tools.clone(),
            runtime: self.runtime_tools.clone(),
            harness: self.harness.clone(),
        }
    }
}

fn tool_name_list(tools: &[Arc<dyn Tool>]) -> String {
    tools
        .iter()
        .map(|tool| tool.name())
        .collect::<Vec<_>>()
        .join(", ")
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
                    "description": "JavaScript source. Runs inside an async function; use `await`, top-level `return`, `ctx.*` tool methods, and `ctx.log`."
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
            .map(Duration::from_millis)
            .unwrap_or(self.limits.default_timeout);

        // The engine runs on a spawned task from the start so promotion never
        // drops a half-executed interpreter: the initial synchronous wait and
        // the background phase observe the same run.
        let stop = CancellationToken::new();
        let logs = Arc::new(Mutex::new(LogBuffer::default()));
        let op_runtime = current_async_operation_runtime();
        let mut join = {
            let namespaces = self.namespaces();
            let limits = self.limits.clone();
            let script = script.to_string();
            let engine_stop = stop.clone();
            let engine_logs = logs.clone();
            let scope_runtime = op_runtime.clone();
            tokio::spawn(async move {
                let run =
                    engine::run(&script, namespaces, &limits, timeout, engine_stop, engine_logs);
                match scope_runtime {
                    Some(runtime) => scope_async_operation_runtime(runtime, run).await,
                    None => run.await,
                }
            })
        };

        let Some(runtime) = op_runtime else {
            // No async-operation runtime in scope: run synchronously. The
            // engine terminates itself at `timeout` via its deadline thread.
            return match join.await {
                Ok(Ok(outcome)) => Ok(synchronous_result(outcome, &logs)),
                Ok(Err(error)) => Ok(ToolResult::failure(error.to_string())),
                Err(error) => Ok(ToolResult::failure(format!(
                    "script task failed: {error}"
                ))),
            };
        };

        match tokio::time::timeout(self.initial_wait, &mut join).await {
            Ok(Ok(Ok(outcome))) => Ok(synchronous_result(outcome, &logs)),
            Ok(Ok(Err(error))) => Ok(ToolResult::failure(error.to_string())),
            Ok(Err(error)) => Ok(ToolResult::failure(format!(
                "script task failed: {error}"
            ))),
            Err(_elapsed) => {
                promote_to_operation(runtime, join, stop, logs, script, timeout).await
            }
        }
    }
}

fn synchronous_result(outcome: ScriptOutcome, logs: &Arc<Mutex<LogBuffer>>) -> ToolResult {
    let drained = logs.lock().expect("log mutex poisoned").drain_all();
    outcome_to_tool_result(&outcome, &drained)
}

/// Shared conversion from an engine outcome to a model-facing tool result.
pub(crate) fn outcome_to_tool_result(outcome: &ScriptOutcome, drained: &[String]) -> ToolResult {
    if let Some(error) = &outcome.error {
        let mut text = error.clone();
        if !drained.is_empty() {
            text.push_str("\n\nscript log:\n");
            text.push_str(&drained.join("\n"));
        }
        return ToolResult::failure(text);
    }
    ToolResult::success(serde_json::to_string(&envelope(outcome, drained)).unwrap_or_default())
}

/// Machine-readable envelope so the model can parse the return value even
/// when log lines are present.
pub(crate) fn envelope(outcome: &ScriptOutcome, drained: &[String]) -> serde_json::Value {
    json!({
        "result": outcome.value.clone().unwrap_or(serde_json::Value::Null),
        "log": drained,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use nenjo::{AsyncOpManager, AsyncOperationRuntime};
    use nenjo_tool_api::ToolCategory;
    use serde_json::Value;
    use std::time::Duration;

    /// Deterministic stand-ins for real worker tools.
    struct FakeTool {
        name: &'static str,
        behavior: FakeBehavior,
    }

    enum FakeBehavior {
        /// Echoes the call arguments back as JSON.
        Echo,
        /// Always returns a denial.
        Deny(&'static str),
        /// Sleeps before echoing — drives promotion past the initial wait.
        Slow(Duration),
    }

    impl FakeTool {
        fn echo(name: &'static str) -> Arc<dyn Tool> {
            Arc::new(Self {
                name,
                behavior: FakeBehavior::Echo,
            })
        }

        fn deny(name: &'static str, reason: &'static str) -> Arc<dyn Tool> {
            Arc::new(Self {
                name,
                behavior: FakeBehavior::Deny(reason),
            })
        }

        fn slow(name: &'static str, delay: Duration) -> Arc<dyn Tool> {
            Arc::new(Self {
                name,
                behavior: FakeBehavior::Slow(delay),
            })
        }
    }

    #[async_trait]
    impl Tool for FakeTool {
        fn name(&self) -> &str {
            self.name
        }

        fn description(&self) -> &str {
            "fake tool for script tests"
        }

        fn parameters_schema(&self) -> Value {
            json!({"type": "object"})
        }

        fn category(&self) -> ToolCategory {
            ToolCategory::Read
        }

        fn origin(&self) -> ToolOrigin {
            ToolOrigin::Mcp
        }

        async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
            match &self.behavior {
                FakeBehavior::Echo => Ok(ToolResult::success(args.to_string())),
                FakeBehavior::Deny(reason) => Ok(ToolResult::failure(*reason)),
                FakeBehavior::Slow(delay) => {
                    tokio::time::sleep(*delay).await;
                    Ok(ToolResult::success(args.to_string()))
                }
            }
        }
    }

    fn script_tool(mcp: Vec<Arc<dyn Tool>>, runtime: Vec<Arc<dyn Tool>>) -> ScriptTool {
        ScriptTool::new(mcp, runtime, None)
    }

    async fn run_script(tool: &ScriptTool, script: &str) -> ToolResult {
        tool.execute(json!({"script": script, "timeout_ms": 8_000}))
            .await
            .expect("execute should not error at the transport level")
    }

    fn envelope_of(result: &ToolResult) -> Value {
        serde_json::from_str(&result.output.text_content()).expect("JSON envelope")
    }

    #[tokio::test]
    async fn script_dispatches_tools_from_both_namespaces() {
        let tool = script_tool(
            vec![FakeTool::echo("mcp_test__get_issue")],
            vec![FakeTool::echo("runtime_test__read_file")],
        );
        let result = run_script(
            &tool,
            r#"
                const issue = await ctx.mcp.mcp_test__get_issue({ id: 42 });
                const file = await ctx.runtime.runtime_test__read_file({ path: "a.txt" });
                ctx.log("fetched");
                return { issue: issue.ok, file: file.content };
            "#,
        )
        .await;
        assert!(result.success, "script failed: {:?}", result.error);
        let value = envelope_of(&result);
        assert_eq!(value["result"]["issue"], json!(true));
        assert_eq!(value["result"]["file"], json!(r#"{"path":"a.txt"}"#));
        assert_eq!(value["log"], json!(["fetched"]));
    }

    #[tokio::test]
    async fn script_surfaces_denials_like_direct_calls() {
        let tool = script_tool(
            vec![FakeTool::deny("mcp_test__write_file", "denied by policy")],
            vec![],
        );
        let result = run_script(
            &tool,
            "return await ctx.mcp.mcp_test__write_file({ path: 'x' });",
        )
        .await;
        assert!(result.success, "script itself succeeds; denial is data");
        let value = envelope_of(&result);
        assert_eq!(value["result"]["ok"], json!(false));
        assert_eq!(value["result"]["error"], json!("denied by policy"));
    }

    #[tokio::test]
    async fn unknown_namespace_method_threads_a_readable_type_error() {
        let tool = script_tool(vec![FakeTool::echo("mcp_test__known")], vec![]);
        let result = run_script(&tool, "return await ctx.mcp.mcp_test__does_not_exist({});").await;
        // The shim only defines methods for granted tools, so a typo'd tool is
        // an ordinary JS TypeError the model can read and fix.
        assert!(!result.success);
        let error = result.error.as_deref().expect("error set");
        assert!(error.contains("not a function"), "unexpected error: {error}");
    }

    #[tokio::test]
    async fn list_and_harness_are_exposed() {
        let tool = ScriptTool::new(
            vec![FakeTool::echo("mcp_test__a")],
            vec![FakeTool::echo("runtime_test__b")],
            Some(ScriptHarnessContext {
                session_id: Some(uuid::Uuid::nil()),
                project_slug: Some("acme".into()),
            }),
        );
        let result = run_script(
            &tool,
            r#"
                return {
                    mcpNames: (await ctx.mcp.list()).map(t => t.name),
                    runtimeNames: (await ctx.runtime.list()).map(t => t.name),
                    project: ctx.harness.project,
                    sessionId: ctx.harness.sessionId,
                    hasLog: typeof ctx.log === "function",
                };
            "#,
        )
        .await;
        assert!(result.success, "script failed: {:?}", result.error);
        let value = envelope_of(&result);
        assert_eq!(value["result"]["mcpNames"], json!(["mcp_test__a"]));
        assert_eq!(value["result"]["runtimeNames"], json!(["runtime_test__b"]));
        assert_eq!(value["result"]["project"], json!("acme"));
        assert_eq!(value["result"]["sessionId"], json!(uuid::Uuid::nil().to_string()));
        assert_eq!(value["result"]["hasLog"], json!(true));
    }

    #[tokio::test]
    async fn busy_loop_is_killed_by_timeout() {
        let tool = script_tool(vec![FakeTool::echo("mcp_test__echo")], vec![]);
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
            result
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("timeout"),
            "unexpected error: {:?}",
            result.error
        );
    }

    #[tokio::test]
    async fn oversized_return_value_is_rejected() {
        let mut tool = script_tool(vec![FakeTool::echo("mcp_test__echo")], vec![]);
        tool.limits.max_output_bytes = 1_000;
        let result = run_script(&tool, "return { blob: 'x'.repeat(100_000) };").await;
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
        let tool = script_tool(vec![FakeTool::echo("mcp_test__echo")], vec![]);
        let result = tool
            .execute(json!({}))
            .await
            .expect("execute should not error at the transport level");
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap_or_default().contains("script"));
    }

    #[tokio::test]
    async fn long_script_is_promoted_to_async_operation_and_completes() {
        // A tool call slower than the initial wait forces promotion.
        let tool = ScriptTool::with_initial_wait(
            vec![FakeTool::slow("mcp_test__slow", Duration::from_millis(800))],
            vec![],
            None,
            Duration::from_millis(200),
        );
        let runtime = AsyncOperationRuntime::new(AsyncOpManager::new());
        let result = scope_async_operation_runtime(runtime.clone(), async {
            tool.execute(json!({"script": r#"
                ctx.log("started");
                return await ctx.mcp.mcp_test__slow({ n: 1 });
            "#}))
            .await
            .expect("execute should not error at the transport level")
        })
        .await;
        assert!(
            result.success,
            "expected promotion receipt: {:?}",
            result.output.text_content()
        );
        let receipt: Value = serde_json::from_str(&result.output.text_content()).unwrap();
        assert_eq!(receipt["type"], json!("operation_started"));
        assert_eq!(receipt["kind"], json!("script"));
        let operation_id = receipt["operation_id"].as_str().expect("operation id").to_string();

        // Let the background run finish, then inspect the settled operation.
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let inspect = runtime
            .inspect(vec![operation_id.clone()], None, true, 10)
            .await;
        let printed = serde_json::to_value(&inspect).unwrap().to_string();
        assert!(printed.contains("Script completed"), "output missing: {printed}");
        assert!(printed.contains(r#"{\"n\":1}"#), "final output missing: {printed}");
        assert!(printed.contains("started"), "streamed log missing: {printed}");
    }

    #[tokio::test]
    async fn stopping_a_promoted_script_transitions_it_to_stopped() {
        let tool = ScriptTool::with_initial_wait(
            vec![FakeTool::slow("mcp_test__slow", Duration::from_secs(60))],
            vec![],
            None,
            Duration::from_millis(200),
        );
        let runtime = AsyncOperationRuntime::new(AsyncOpManager::new());
        let result = scope_async_operation_runtime(runtime.clone(), async {
            tool.execute(json!({"script": "ctx.log('looping'); while (true) { await ctx.mcp.mcp_test__slow({}); }", "timeout_ms": 60_000}))
                .await
                .expect("execute should not error")
        })
        .await;
        assert!(result.success, "expected promotion receipt");
        let receipt: Value = serde_json::from_str(&result.output.text_content()).unwrap();
        let operation_id = receipt["operation_id"].as_str().expect("operation id").to_string();

        let stopped = runtime
            .stop(vec![operation_id.clone()], None, Some("test stop".into()), None)
            .await;
        let printed = serde_json::to_value(&stopped).unwrap().to_string();
        assert!(
            printed.contains(&operation_id) && printed.contains("stopped"),
            "operation should transition to stopped: {printed}"
        );

        // The engine must exit promptly via the stop→token bridge.
        tokio::time::sleep(Duration::from_millis(500)).await;
        let inspect = runtime
            .inspect(vec![operation_id.clone()], None, false, 10)
            .await;
        let printed = serde_json::to_value(&inspect).unwrap().to_string();
        assert!(printed.contains("\"stopped\""), "expected stopped status: {printed}");
    }
}
