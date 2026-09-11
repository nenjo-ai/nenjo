//! Model-facing inspect, input, stop, and wait controls for every operation kind.

use std::sync::Arc;

use anyhow::Result;

use crate::agents::runner::turn_loop;
use crate::tools::{
    AsyncControl, AsyncControlResult, INSPECT_TOOL_NAME, InspectOperationsArgs,
    SEND_INPUT_TOOL_NAME, STOP_TOOL_NAME, SendOperationInputArgs, StopOperationsArgs, Tool,
    ToolCategory, ToolOrigin, ToolResult, WAIT_TOOL_NAME, WaitOperationsArgs,
    inspect_operations_parameters_schema, send_operation_input_parameters_schema,
    stop_operations_parameters_schema, wait_operations_parameters_schema,
};

use super::{AsyncOpManager, AsyncOpWaitFilter, AsyncOpWaitResult};

struct InspectOperationsTool {
    async_ops: AsyncOpManager,
}

struct StopOperationsTool {
    async_ops: AsyncOpManager,
}

struct SendOperationInputTool {
    async_ops: AsyncOpManager,
}

struct WaitOperationsTool {
    async_ops: AsyncOpManager,
}

#[async_trait::async_trait]
impl Tool for InspectOperationsTool {
    fn name(&self) -> &str {
        INSPECT_TOOL_NAME
    }

    fn description(&self) -> &str {
        "Inspect running or recently completed async operations by operation_id. Use this after wait reports completion or failure when you need the final output payload or recent transcript."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        inspect_operations_parameters_schema()
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Read
    }

    fn origin(&self) -> ToolOrigin {
        ToolOrigin::Harness
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let parsed: InspectOperationsArgs = serde_json::from_value(args)?;
        Ok(json_tool(serde_json::json!(
            self.async_ops
                .inspect(
                    parsed.operations,
                    parsed.kind,
                    parsed.include_transcript,
                    parsed.limit,
                )
                .await
        )))
    }
}

#[async_trait::async_trait]
impl Tool for StopOperationsTool {
    fn name(&self) -> &str {
        STOP_TOOL_NAME
    }

    fn description(&self) -> &str {
        "Stop one or more running async operations. Filter by kind to avoid stopping unrelated work. Do not stop and restart an operation solely because an internal tool call failed while its status remains running."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        stop_operations_parameters_schema()
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::ReadWrite
    }

    fn origin(&self) -> ToolOrigin {
        ToolOrigin::Harness
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let parsed: StopOperationsArgs = serde_json::from_value(args)?;
        Ok(json_tool(serde_json::json!(
            self.async_ops
                .stop(
                    parsed.operations,
                    parsed.kind,
                    parsed.reason,
                    turn_loop::current_events_tx(),
                )
                .await
        )))
    }
}

#[async_trait::async_trait]
impl Tool for SendOperationInputTool {
    fn name(&self) -> &str {
        SEND_INPUT_TOOL_NAME
    }

    fn description(&self) -> &str {
        "Send input to one or more async operations that asked the parent agent a question."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        send_operation_input_parameters_schema()
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::ReadWrite
    }

    fn origin(&self) -> ToolOrigin {
        ToolOrigin::Harness
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let parsed: SendOperationInputArgs = serde_json::from_value(args)?;
        Ok(json_tool(serde_json::json!(
            self.async_ops
                .send_input(parsed.operations, parsed.message)
                .await
        )))
    }
}

#[async_trait::async_trait]
impl Tool for WaitOperationsTool {
    fn name(&self) -> &str {
        WAIT_TOOL_NAME
    }

    fn description(&self) -> &str {
        "Wait while async operations continue running, then return queued operation signals. A recoverable_tool_error keeps the operation running and recommends waiting rather than stopping and reinvoking it. For video or other media generation, call this with kind=media after the media tool returns job_started; repeat wait until the media operation completes or fails instead of calling the generation tool again for the same request."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        wait_operations_parameters_schema()
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Read
    }

    fn origin(&self) -> ToolOrigin {
        ToolOrigin::Harness
    }

    /// Wait for a matching operation signal or newly queued user input.
    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let parsed: WaitOperationsArgs = serde_json::from_value(args)?;
        let result = wait_for_operations(&self.async_ops, parsed).await;
        Ok(json_tool(serde_json::json!(result)))
    }
}

/// Return immediately without a match, otherwise await operations or user interruption.
///
/// This harness control must remain outside runnable admission. Holding a
/// descendant permit here could prevent the awaited child from making progress.
async fn wait_for_operations(
    manager: &AsyncOpManager,
    args: WaitOperationsArgs,
) -> AsyncControlResult<AsyncOpWaitResult> {
    if !manager
        .has_open_model_visible_matching(args.kind, AsyncControl::Wait)
        .await
    {
        return AsyncControlResult::no_matching_operations();
    }
    let wait = manager.wait(
        args.seconds,
        AsyncOpWaitFilter::control(AsyncControl::Wait, args.kind),
    );
    let result = if let Some(turn_input) = turn_loop::current_turn_input() {
        tokio::select! {
            result = wait => result,
            _ = turn_input.notified() => AsyncOpWaitResult {
                elapsed_seconds: 0,
                woken_by: "user_message",
                updates: Vec::new(),
            },
        }
    } else {
        wait.await
    };
    AsyncControlResult::from_parts(vec![result], Vec::new())
}

/// Build a fixed control surface regardless of which operations are currently active.
///
/// Selection and supported-control checks happen during execution, keeping tool
/// schemas stable across operation lifecycles and model prompt-cache boundaries.
pub(crate) fn build_async_operation_tools(async_ops: AsyncOpManager) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(InspectOperationsTool {
            async_ops: async_ops.clone(),
        }) as Arc<dyn Tool>,
        Arc::new(SendOperationInputTool {
            async_ops: async_ops.clone(),
        }) as Arc<dyn Tool>,
        Arc::new(StopOperationsTool {
            async_ops: async_ops.clone(),
        }) as Arc<dyn Tool>,
        Arc::new(WaitOperationsTool { async_ops }) as Arc<dyn Tool>,
    ]
}

/// Encode the shared operation-control receipt as a successful tool invocation.
fn json_tool(value: serde_json::Value) -> ToolResult {
    ToolResult {
        success: true,
        output: value.to_string().into(),
        error: None,
    }
}
