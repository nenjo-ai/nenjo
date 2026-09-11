//! Tests for ability execution behavior.

use super::*;

#[tokio::test]
async fn prompt_failure_completes_the_operation_and_emits_one_parent_completion() {
    let instance = Arc::new(test_instance_with_active_domain());
    let manager = instance.runtime.async_ops.clone();
    let ability = AbilityManifest::builder()
        .with_slug(crate::Slug::derive("broken-prompt"))
        .with_name("broken-prompt")
        .with_prompt("{{ args.missing }}")
        .build()
        .unwrap();
    let started = manager
        .start_nested(
            StartAsyncOp {
                id: AsyncOpId::new("broken-prompt"),
                kind: AsyncOpKind::Ability,
                label: ability.name.clone(),
                parent_operation_id: None,
                parent_tool_name: Some(USE_ABILITY_TOOL_NAME.into()),
                started_summary: "Run the ability".into(),
                model_visible: true,
                controls: AsyncControls::new(AsyncControl::Inspect),
            },
            None,
        )
        .await
        .unwrap();
    let (events, mut received) = mpsc::unbounded_channel();
    run_ability_operation(AbilityOperation {
        instance,
        ability,
        call_id: "broken-prompt".into(),
        task_description: "Run the ability".into(),
        caller_history_snapshot: Vec::new(),
        timezone: chrono_tz::UTC,
        child_handle: started.child,
        op_handle: started.handle,
        parent_events_tx: Some(events),
    })
    .await;
    let inspection = manager
        .inspect(vec!["broken-prompt".into()], None, false, 1)
        .await;
    assert_eq!(inspection[0].status, "failed");
    let completions: Vec<_> = std::iter::from_fn(|| received.try_recv().ok())
        .filter_map(|event| match event {
            TurnEvent::AbilityCompleted {
                success,
                final_output,
                ..
            } => Some((success, final_output)),
            _ => None,
        })
        .collect();
    assert_eq!(completions.len(), 1);
    assert!(!completions[0].0);
    assert!(completions[0].1.starts_with("ability prompt build failed:"));
}

#[tokio::test]
async fn ability_resumes_after_mcp_result_and_capacity_wait_with_one_descendant_slot() {
    let capacity = AdmissionPool::new("test model", 1, 8, Duration::from_secs(5));
    let provider = Arc::new(SequentialProvider {
        responses: vec![
            ChatResponse {
                text: None,
                tool_calls: vec![ToolCall {
                    id: "bookmarks".into(),
                    name: "mcp_get_users_bookmarks".into(),
                    arguments: "{}".into(),
                }],
                provider_tool_calls: Vec::new(),
                usage: TokenUsage::default(),
                finish_reason: nenjo_models::FinishReason::ToolCalls,
            },
            ChatResponse {
                text: None,
                tool_calls: vec![ToolCall {
                    id: "finish".into(),
                    name: FINISH_ABILITY_TOOL_NAME.into(),
                    arguments: serde_json::json!({
                        "status": "completed",
                        "summary": "Read the bookmark result",
                        "result": {"id": "bookmark-1"}
                    })
                    .to_string(),
                }],
                provider_tool_calls: Vec::new(),
                usage: TokenUsage::default(),
                finish_reason: nenjo_models::FinishReason::ToolCalls,
            },
        ],
        next: AtomicUsize::new(0),
        seen_messages: Mutex::new(Vec::new()),
    });
    let (occupied_tx, mut occupied_rx) = mpsc::unbounded_channel();
    let mut instance = test_instance_with_active_domain();
    instance.model.model_provider = Arc::new(QueuedAbilityProvider {
        inner: provider.clone(),
        capacity: capacity.clone(),
    });
    instance.runtime.provider_runtime = Some(test_sdk_provider_with_tools(Arc::new(
        BookmarkToolFactory(Arc::new(BookmarkTool {
            capacity: capacity.clone(),
            occupied: occupied_tx,
        })),
    )));
    let instance = Arc::new(instance);
    let manager = instance.runtime.async_ops.clone();
    let ability = AbilityManifest {
        slug: crate::Slug::derive("bookmarks"),
        name: "bookmarks".into(),
        path: None,
        description: None,
        activation_condition: "When bookmarks are requested".into(),
        prompt_config: AbilityPromptConfig {
            developer_prompt: "Read bookmarks and finish.".into(),
        },
        platform_scopes: Vec::new(),
        mcp_servers: Vec::new(),
        script_tools: Vec::new(),
        media: Vec::new(),
        source_type: "native".into(),
        read_only: true,
        metadata: serde_json::Value::Null,
    };
    let (events_tx, mut events_rx) = mpsc::unbounded_channel();
    let root =
        ExecutionContext::root(1, 1, 8, Duration::from_secs(5)).with_events(events_tx.clone());

    let exercise = root.scope(async {
        let operation_id = "ability_bookmarks_resume".to_string();
        let request = StartAsyncOp {
            id: AsyncOpId::new(operation_id.clone()),
            kind: AsyncOpKind::Ability,
            label: "bookmarks".into(),
            parent_operation_id: None,
            parent_tool_name: Some(USE_ABILITY_TOOL_NAME.into()),
            started_summary: "Fetch bookmarks".into(),
            model_visible: true,
            controls: AsyncControls::new(AsyncControl::Inspect).with(AsyncControl::Wait),
        };
        let started = manager
            .start_nested(request, Some(events_tx.clone()))
            .await
            .unwrap();
        let operation = AbilityOperation {
            instance,
            ability,
            call_id: operation_id.clone(),
            task_description: "Fetch bookmarks".into(),
            caller_history_snapshot: Vec::new(),
            timezone: chrono_tz::UTC,
            child_handle: started.child,
            op_handle: started.handle.clone(),
            parent_events_tx: Some(events_tx.clone()),
        };
        let join = tokio::spawn(crate::concurrency::in_scope(
            Some(root.child()),
            run_ability_operation(operation),
        ));
        started.handle.attach_join(join, Some(events_tx)).await;
        let held = occupied_rx.recv().await.unwrap();
        let mut waiting_for_model = false;
        let mut returned_bookmarks = false;
        while !waiting_for_model || !returned_bookmarks {
            let event = events_rx.recv().await.unwrap();
            waiting_for_model |= matches!(
                &event, TurnEvent::ResourceCapacityWaiting { pool, .. } if pool == "test model"
            );
            returned_bookmarks |= matches!(
                &event, TurnEvent::ToolCallEnd { tool_name, .. }
                    if tool_name == "mcp_get_users_bookmarks"
            );
        }
        let inspection = manager
            .inspect(
                vec![operation_id.clone()],
                Some(AsyncOpKind::Ability),
                true,
                20,
            )
            .await;
        assert_eq!(inspection[0].status, "running");
        let transcript = inspection[0].transcript_delta.as_ref().unwrap();
        assert!(transcript.iter().any(|event| matches!(
            event, AsyncOperationTranscriptEvent::ToolResult { tool, success: true, .. }
                if tool == "mcp_get_users_bookmarks"
        )));
        manager
            .drain_signals(AsyncOpWaitFilter::model_visible())
            .await;
        let controls = build_async_operation_tools(manager.clone());
        let wait_tool = controls.iter().find(|tool| tool.name() == "wait").unwrap();
        let wait_args = serde_json::json!({"kind": "ability", "seconds": 30});
        let mut wait = Box::pin(wait_tool.execute(wait_args));
        assert!(futures_util::poll!(wait.as_mut()).is_pending());
        drop(held);
        let result = wait.await.unwrap();
        let result: serde_json::Value =
            serde_json::from_str(&result.output.text_content()).unwrap();
        assert_eq!(result["status"], "applied");
        assert_eq!(result["rejected"], serde_json::json!([]));
        let inspection = manager
            .inspect(vec![operation_id], Some(AsyncOpKind::Ability), false, 20)
            .await;
        assert_eq!(inspection[0].status, "completed");
        let output = inspection[0].latest_output.as_ref().unwrap();
        assert_eq!(output["result"]["id"], "bookmark-1");
    });
    tokio::time::timeout(Duration::from_secs(2), exercise)
        .await
        .expect("ability did not resume after model capacity was released");

    capacity.acquire().await.unwrap();
    let seen = provider.seen_messages.lock().unwrap();
    assert_eq!(seen.len(), 2);
    assert!(seen[1].iter().any(|message| {
        matches!(message, ConversationMessage::ToolResults(results) if results.iter().any(|result| result.output.text_content().contains("bookmark-1")))
    }));
}

#[tokio::test]
async fn failed_finish_propagates_to_async_state_and_parent_event() {
    let provider = Arc::new(SequentialProvider {
        responses: vec![ChatResponse {
            text: None,
            tool_calls: vec![ToolCall {
                id: "finish_failed".into(),
                name: FINISH_ABILITY_TOOL_NAME.into(),
                arguments: serde_json::json!({
                    "status": "failed",
                    "summary": "Routine verification failed",
                    "result": {"slug": "code-generation", "verified": false}
                })
                .to_string(),
            }],
            provider_tool_calls: Vec::new(),
            usage: TokenUsage::default(),
            finish_reason: nenjo_models::FinishReason::Stop,
        }],
        next: AtomicUsize::new(0),
        seen_messages: Mutex::new(Vec::new()),
    });
    let mut instance = test_instance_with_active_domain();
    instance.model.model_provider = provider;
    let instance = Arc::new(instance);
    let manager = instance.runtime.async_ops.clone();
    let operation_id = AsyncOpId::new("ability_build_routine_failed");
    let controls = AsyncControls::new(AsyncControl::Inspect).with(AsyncControl::Wait);
    let started = manager
        .start(
            StartAsyncOp {
                id: operation_id.clone(),
                kind: AsyncOpKind::Ability,
                label: "build_routine".into(),
                parent_operation_id: None,
                parent_tool_name: Some(USE_ABILITY_TOOL_NAME.into()),
                started_summary: "Build the routine".into(),
                model_visible: true,
                controls,
            },
            None,
        )
        .await;
    manager
        .drain_signals(AsyncOpWaitFilter::model_visible())
        .await;
    let (events_tx, mut events_rx) = mpsc::unbounded_channel();
    let ability = AbilityManifest {
        slug: crate::Slug::derive("build_routine"),
        name: "build_routine".into(),
        path: None,
        description: Some("Build a routine".into()),
        activation_condition: "When a routine is requested".into(),
        prompt_config: AbilityPromptConfig {
            developer_prompt: "Build and verify the requested routine.".into(),
        },
        platform_scopes: Vec::new(),
        mcp_servers: Vec::new(),
        script_tools: Vec::new(),
        media: Vec::new(),
        source_type: "native".into(),
        read_only: false,
        metadata: serde_json::Value::Null,
    };

    run_ability_operation(AbilityOperation {
        instance,
        ability,
        call_id: operation_id.to_string(),
        task_description: "Build the routine".into(),
        caller_history_snapshot: Vec::new(),
        timezone: chrono_tz::UTC,
        child_handle: started.child,
        op_handle: started.handle,
        parent_events_tx: Some(events_tx),
    })
    .await;

    let signals = manager
        .drain_signals(AsyncOpWaitFilter::model_visible())
        .await;
    assert!(signals.iter().flat_map(|digest| &digest.events).any(
        |signal| matches!(signal, AsyncOpSignal::Failed { error, .. } if error == "Routine verification failed")
    ));
    let inspections = manager
        .inspect(
            vec![operation_id.to_string()],
            Some(AsyncOpKind::Ability),
            true,
            10,
        )
        .await;
    assert_eq!(inspections.len(), 1);
    assert_eq!(inspections[0].status, "failed");
    assert_eq!(
        inspections[0]
            .latest_output
            .as_ref()
            .and_then(|output| output.pointer("/result/verified")),
        Some(&serde_json::Value::Bool(false))
    );
    assert!(
        std::iter::from_fn(|| events_rx.try_recv().ok()).any(|event| {
            matches!(
                event,
                TurnEvent::AbilityCompleted {
                    success: false,
                    final_output,
                    ..
                } if final_output == "Routine verification failed"
            )
        })
    );
}

#[tokio::test]
async fn failed_ability_tool_call_emits_a_recoverable_running_signal() {
    let manager = AsyncOpManager::new();
    let started = manager
        .start(
            StartAsyncOp {
                id: AsyncOpId::new("ability_build_routine_1"),
                kind: AsyncOpKind::Ability,
                label: "build_routine".into(),
                parent_operation_id: None,
                parent_tool_name: Some(USE_ABILITY_TOOL_NAME.into()),
                started_summary: "building routine".into(),
                model_visible: true,
                controls: AsyncControls::new(AsyncControl::Inspect).with(AsyncControl::Wait),
            },
            None,
        )
        .await;
    manager
        .drain_signals(AsyncOpWaitFilter::model_visible())
        .await;

    bridge_ability_transcript(
        &started.handle,
        &TurnEvent::ToolCallEnd {
            batch_id: "batch_1".into(),
            parent_tool_name: None,
            tool_call_id: Some("call_1".into()),
            tool_name: "configure_routine".into(),
            tool_args: "{}".into(),
            result: ToolResult {
                success: false,
                output: String::new().into(),
                error: Some("graph validation failed".into()),
            },
            metadata: None,
        },
        None,
    )
    .await;

    let result = manager
        .wait(1, AsyncOpWaitFilter::kind(Some(AsyncOpKind::Ability)))
        .await;
    assert_eq!(result.woken_by, "recoverable_error");
    assert_eq!(result.updates[0].status, "running");
    assert!(matches!(
        &result.updates[0].events[0],
        AsyncOpSignal::RecoverableToolError { tool, error, .. }
            if tool == "configure_routine" && error == "graph validation failed"
    ));
}
