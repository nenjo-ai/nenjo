//! Tests for ability completion behavior.

use super::*;

#[tokio::test]
async fn ability_completion_rejects_plain_prose_until_finish_is_called() {
    let provider = Arc::new(SequentialProvider {
        responses: vec![
            ChatResponse {
                text: Some("The prerequisites exist. I'll create it now.".into()),
                tool_calls: Vec::new(),
                provider_tool_calls: Vec::new(),
                usage: TokenUsage::default(),
                finish_reason: nenjo_models::FinishReason::Stop,
            },
            ChatResponse {
                text: None,
                tool_calls: vec![ToolCall {
                    id: "finish_1".into(),
                    name: FINISH_ABILITY_TOOL_NAME.into(),
                    arguments: serde_json::json!({
                        "status": "completed",
                        "summary": "Created and verified the routine",
                        "result": {"slug": "code-generation"}
                    })
                    .to_string(),
                }],
                provider_tool_calls: Vec::new(),
                usage: TokenUsage::default(),
                finish_reason: nenjo_models::FinishReason::Stop,
            },
        ],
        next: AtomicUsize::new(0),
        seen_messages: Mutex::new(Vec::new()),
    });
    let mut instance = test_instance_with_active_domain();
    instance.model.model_provider = provider.clone();
    instance.runtime.tools = vec![Arc::new(FinishAbilityTool)];

    let output = turn_loop::run(
        &instance,
        vec![ConversationMessage::user("Create the routine")],
        None,
        None,
        None,
        turn_loop::TurnCompletion::RequireTool(FINISH_ABILITY_TOOL_NAME),
        crate::agents::runner::chat::ProviderResponseDelivery::Buffered,
    )
    .await
    .unwrap();

    let finish: AbilityFinish = serde_json::from_str(&output.text).unwrap();
    assert!(matches!(finish.status, AbilityFinishStatus::Completed));
    assert_eq!(finish.summary, "Created and verified the routine");
    assert_eq!(output.tool_calls, 1);

    let seen_messages = provider.seen_messages.lock().unwrap();
    assert_eq!(seen_messages.len(), 2);
    assert!(seen_messages[1].iter().any(|message| {
        message.as_chat().is_some_and(|chat| {
            chat.role == nenjo_models::ChatRole::Developer
                && chat.content.contains("requires the finish tool")
                && chat.content.contains("I'll create it now")
        })
    }));
}

#[tokio::test]
async fn required_finish_uses_its_own_result_when_another_terminal_tool_runs_first() {
    let provider = Arc::new(SequentialProvider {
        responses: vec![ChatResponse {
            text: None,
            tool_calls: vec![
                ToolCall {
                    id: "other_1".into(),
                    name: "other_terminal".into(),
                    arguments: "{}".into(),
                },
                ToolCall {
                    id: "finish_1".into(),
                    name: FINISH_ABILITY_TOOL_NAME.into(),
                    arguments: serde_json::json!({
                        "status": "completed",
                        "summary": "Created the routine",
                        "result": {"slug": "code-generation"}
                    })
                    .to_string(),
                },
            ],
            provider_tool_calls: Vec::new(),
            usage: TokenUsage::default(),
            finish_reason: nenjo_models::FinishReason::Stop,
        }],
        next: AtomicUsize::new(0),
        seen_messages: Mutex::new(Vec::new()),
    });
    let mut instance = test_instance_with_active_domain();
    instance.model.model_provider = provider;
    instance.runtime.tools = vec![Arc::new(OtherTerminalTool), Arc::new(FinishAbilityTool)];

    let output = turn_loop::run(
        &instance,
        vec![ConversationMessage::user("Create the routine")],
        None,
        None,
        None,
        turn_loop::TurnCompletion::RequireTool(FINISH_ABILITY_TOOL_NAME),
        crate::agents::runner::chat::ProviderResponseDelivery::Buffered,
    )
    .await
    .unwrap();

    let finish: AbilityFinish = serde_json::from_str(&output.text).unwrap();
    assert!(matches!(finish.status, AbilityFinishStatus::Completed));
    assert_eq!(finish.summary, "Created the routine");
    assert_eq!(output.tool_calls, 2);
}

#[tokio::test]
async fn invalid_finish_result_is_recoverable_within_the_same_ability_run() {
    let provider = Arc::new(SequentialProvider {
        responses: vec![
            ChatResponse {
                text: None,
                tool_calls: vec![ToolCall {
                    id: "finish_invalid".into(),
                    name: FINISH_ABILITY_TOOL_NAME.into(),
                    arguments: serde_json::json!({
                        "status": "completed",
                        "summary": " "
                    })
                    .to_string(),
                }],
                provider_tool_calls: Vec::new(),
                usage: TokenUsage::default(),
                finish_reason: nenjo_models::FinishReason::Stop,
            },
            ChatResponse {
                text: None,
                tool_calls: vec![ToolCall {
                    id: "finish_valid".into(),
                    name: FINISH_ABILITY_TOOL_NAME.into(),
                    arguments: serde_json::json!({
                        "status": "completed",
                        "summary": "Created and verified the routine"
                    })
                    .to_string(),
                }],
                provider_tool_calls: Vec::new(),
                usage: TokenUsage::default(),
                finish_reason: nenjo_models::FinishReason::Stop,
            },
        ],
        next: AtomicUsize::new(0),
        seen_messages: Mutex::new(Vec::new()),
    });
    let mut instance = test_instance_with_active_domain();
    instance.model.model_provider = provider.clone();
    instance.runtime.tools = vec![Arc::new(FinishAbilityTool)];

    let output = turn_loop::run(
        &instance,
        vec![ConversationMessage::user("Create the routine")],
        None,
        None,
        None,
        turn_loop::TurnCompletion::RequireTool(FINISH_ABILITY_TOOL_NAME),
        crate::agents::runner::chat::ProviderResponseDelivery::Buffered,
    )
    .await
    .unwrap();

    let finish: AbilityFinish = serde_json::from_str(&output.text).unwrap();
    assert_eq!(finish.summary, "Created and verified the routine");
    let seen_messages = provider.seen_messages.lock().unwrap();
    assert_eq!(seen_messages.len(), 2);
    assert!(seen_messages[1].iter().any(|message| {
        matches!(message, ConversationMessage::ToolResults(results)
            if results.iter().any(|result| result.output.contains("summary is required")))
    }));
}

#[tokio::test]
async fn required_completion_tool_must_be_registered_and_terminal() {
    let mut instance = test_instance_with_active_domain();
    instance.runtime.tools.clear();

    let error = turn_loop::run(
        &instance,
        vec![ConversationMessage::user("Create the routine")],
        None,
        None,
        None,
        turn_loop::TurnCompletion::RequireTool(FINISH_ABILITY_TOOL_NAME),
        crate::agents::runner::chat::ProviderResponseDelivery::Buffered,
    )
    .await
    .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("required completion tool 'finish'")
    );
}

#[tokio::test]
async fn finish_requires_a_nonempty_summary() {
    let result = FinishAbilityTool
        .execute(serde_json::json!({
            "status": "completed",
            "summary": "  "
        }))
        .await
        .unwrap();

    assert!(!result.success);
    assert_eq!(result.error.as_deref(), Some("summary is required"));
}
