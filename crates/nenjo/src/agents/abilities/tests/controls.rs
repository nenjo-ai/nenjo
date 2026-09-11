//! Tests for ability controls behavior.

use super::*;

#[test]
fn async_operation_tools_are_generic_harness_tools() {
    let tools = build_async_operation_tools(AsyncOpManager::new());
    let names: Vec<_> = tools.iter().map(|tool| tool.name()).collect();

    assert_eq!(names, ["inspect", "send_input", "stop", "wait"]);
}

#[tokio::test]
async fn generic_async_controls_are_fixed_before_during_and_after_operations() {
    assert_eq!(
        visible_generic_control_names(None).await,
        ["inspect", "send_input", "stop", "wait"]
    );
    let all_controls = AsyncControls::new(AsyncControl::Inspect)
        .with(AsyncControl::SendInput)
        .with(AsyncControl::Stop)
        .with(AsyncControl::Wait);
    for kind in [
        AsyncOpKind::Ability,
        AsyncOpKind::SubAgent,
        AsyncOpKind::Delegation,
    ] {
        assert_eq!(
            visible_generic_control_names(Some((kind, all_controls))).await,
            ["inspect", "send_input", "stop", "wait"]
        );
    }
    assert_eq!(
        visible_generic_control_names(Some((
            AsyncOpKind::Media,
            AsyncControls::new(AsyncControl::Inspect)
                .with(AsyncControl::Stop)
                .with(AsyncControl::Wait),
        )))
        .await,
        ["inspect", "send_input", "stop", "wait"]
    );
}

#[tokio::test]
async fn serialized_async_control_specs_do_not_change_across_all_operation_lifecycles() {
    let mut instance = test_instance_with_active_domain();
    let async_ops = instance.runtime.async_ops.clone();
    instance.runtime.tools = build_async_operation_tools(async_ops.clone());
    let baseline = serde_json::to_vec(&instance.visible_local_tool_specs().await).unwrap();

    for (index, kind) in [
        AsyncOpKind::Ability,
        AsyncOpKind::Delegation,
        AsyncOpKind::SubAgent,
        AsyncOpKind::Shell,
        AsyncOpKind::Media,
        AsyncOpKind::TaskExecution,
    ]
    .into_iter()
    .enumerate()
    {
        let controls = if matches!(
            kind,
            AsyncOpKind::Ability | AsyncOpKind::Delegation | AsyncOpKind::SubAgent
        ) {
            AsyncControls::new(AsyncControl::Inspect)
                .with(AsyncControl::SendInput)
                .with(AsyncControl::Stop)
                .with(AsyncControl::Wait)
        } else {
            AsyncControls::new(AsyncControl::Inspect)
                .with(AsyncControl::Stop)
                .with(AsyncControl::Wait)
        };
        let started = async_ops
            .start(
                StartAsyncOp {
                    id: AsyncOpId::new(format!("{}-{index}", kind.as_str())),
                    kind,
                    label: kind.as_str().into(),
                    parent_operation_id: None,
                    parent_tool_name: Some("test".into()),
                    started_summary: "started".into(),
                    model_visible: true,
                    controls,
                },
                None,
            )
            .await;
        assert_eq!(
            serde_json::to_vec(&instance.visible_local_tool_specs().await).unwrap(),
            baseline
        );

        match index % 3 {
            0 => {
                started
                    .handle
                    .complete(
                        AsyncOpSignal::Completed {
                            summary: "done".into(),
                            output: None,
                        },
                        None,
                    )
                    .await;
            }
            1 => {
                started
                    .handle
                    .complete(
                        AsyncOpSignal::Failed {
                            error: "failed".into(),
                            output: None,
                        },
                        None,
                    )
                    .await;
            }
            _ => {
                async_ops
                    .stop(
                        vec![format!("{}-{index}", kind.as_str())],
                        Some(kind),
                        Some("stopped".into()),
                        None,
                    )
                    .await;
            }
        }
        assert_eq!(
            serde_json::to_vec(&instance.visible_local_tool_specs().await).unwrap(),
            baseline
        );
    }
}

#[tokio::test]
async fn child_execution_mode_does_not_register_async_control_surface() {
    let mut instance = test_instance_with_active_domain();
    instance.runtime.execution_mode = AgentExecutionMode::Ability;
    instance.runtime.tools.clear();
    let runner = crate::agents::runner::AgentRunner::<ErasedProvider>::new(instance, None, None)
        .await
        .unwrap();
    let names = runner
        .instance()
        .local_tool_specs()
        .into_iter()
        .map(|tool| tool.name)
        .collect::<Vec<_>>();
    assert!(
        !names
            .iter()
            .any(|name| { matches!(name.as_str(), "inspect" | "send_input" | "stop" | "wait") })
    );
}

#[tokio::test]
async fn unsupported_model_control_is_machine_readable() {
    let async_ops = AsyncOpManager::new();
    let tools = build_async_operation_tools(async_ops.clone());
    async_ops
        .start(
            StartAsyncOp {
                id: AsyncOpId::new("media-1"),
                kind: AsyncOpKind::Media,
                label: "media".into(),
                parent_operation_id: None,
                parent_tool_name: Some("generate_image".into()),
                started_summary: "started".into(),
                model_visible: true,
                controls: AsyncControls::new(AsyncControl::Inspect)
                    .with(AsyncControl::Stop)
                    .with(AsyncControl::Wait),
            },
            None,
        )
        .await;
    let send_input = tools
        .iter()
        .find(|tool| tool.name() == SEND_INPUT_TOOL_NAME)
        .unwrap();

    let result = send_input
        .execute(serde_json::json!({
            "operations": ["media-1"],
            "message": "continue"
        }))
        .await
        .unwrap();
    let output: serde_json::Value = serde_json::from_str(result.output.as_text().unwrap()).unwrap();
    assert_eq!(output["status"], "no_matching_operations");
    assert_eq!(output["results"], serde_json::json!([]));
    assert_eq!(output["rejected"][0]["operation_id"], "media-1");
    assert_eq!(output["rejected"][0]["reason"], "unsupported_control");
}

async fn visible_generic_control_names(
    operation: Option<(AsyncOpKind, AsyncControls)>,
) -> Vec<String> {
    let mut instance = test_instance_with_active_domain();
    let async_ops = instance.runtime.async_ops.clone();
    instance.runtime.tools = build_async_operation_tools(async_ops.clone());
    if let Some((kind, controls)) = operation {
        start_model_visible_operation(&async_ops, kind, controls).await;
    }
    instance
        .visible_local_tool_specs()
        .await
        .into_iter()
        .map(|spec| spec.name)
        .collect()
}

async fn start_model_visible_operation(
    async_ops: &AsyncOpManager,
    kind: AsyncOpKind,
    controls: AsyncControls,
) {
    let operation_id = format!("{}_1", kind.as_str());
    let _started = async_ops
        .start(
            StartAsyncOp {
                id: AsyncOpId::new(operation_id),
                kind,
                label: kind.as_str().into(),
                parent_operation_id: None,
                parent_tool_name: Some("starter".into()),
                started_summary: "started".into(),
                model_visible: true,
                controls,
            },
            None,
        )
        .await;
}
