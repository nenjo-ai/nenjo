//! Regression coverage for terminal lifecycle, partial calls, and delta separation.

use serde_json::json;
use tokio::sync::mpsc;

use super::*;

fn completed() -> Value {
    json!({"status":"completed","output":[
        {"type":"reasoning","content":[{"type":"reasoning_text","text":"private reasoning"}]},
        {"type":"message","content":[{"type":"output_text","text":"Hello "},{"type":"output_text","text":"🌍"}]},
        {"type":"function_call","call_id":"call-1","name":"inspect","arguments":"{\"kind\":\"ability\"}","status":"completed"}
    ],"usage":{"input_tokens":25,"output_tokens":8,
        "input_tokens_details":{"cached_tokens":12},
        "output_tokens_details":{"reasoning_tokens":3}}})
}

#[test]
fn terminal_output_preserves_all_text_call_identity_and_usage() {
    let response = completed_response(completed()).unwrap();
    assert_eq!(response.text.as_deref(), Some("Hello 🌍"));
    assert_eq!(response.tool_calls.len(), 1);
    assert_eq!(response.tool_calls[0].id, "call-1");
    assert_eq!(response.usage.input_tokens, 25);
    assert_eq!(response.usage.output_tokens, 8);
    assert_eq!(response.usage.cached_input_tokens, Some(12));
    assert_eq!(response.usage.reasoning_tokens, Some(3));
    assert_eq!(response.finish_reason, FinishReason::ToolCalls);
}

#[tokio::test]
async fn terminal_errors_preserve_reason_identity_and_usage_in_both_wire_modes() {
    for (status, detail, reason) in [
        (
            "incomplete",
            "max_output_tokens",
            ResponseTermination::OutputLimit,
        ),
        (
            "incomplete",
            "content_filter",
            ResponseTermination::ContentFilter,
        ),
        (
            "incomplete",
            "future_reason",
            ResponseTermination::Incomplete,
        ),
        ("failed", "server_error", ResponseTermination::Failed),
        ("cancelled", "cancelled", ResponseTermination::Cancelled),
    ] {
        let mut payload = completed();
        payload["id"] = json!("resp-123");
        payload["status"] = json!(status);
        payload["incomplete_details"] = json!({"reason":detail});
        // Even seemingly valid calls in an incomplete final payload must not execute.
        let buffered_error = completed_response(payload.clone()).unwrap_err();
        let frame = format!(
            "data: {}",
            json!({"type":format!("response.{status}"),"response":payload})
        );
        let streamed_error =
            absorb_frame(frame.as_bytes(), None, &mut ResponseDiagnostics::default())
                .await
                .unwrap_err();
        for error in [buffered_error, streamed_error] {
            let error = error.downcast_ref::<ResponseTerminationError>().unwrap();
            assert_eq!(error.reason, reason);
            assert_eq!(error.response_id.as_deref(), Some("resp-123"));
            assert_eq!(error.usage.input_tokens, 25);
            assert_eq!(error.usage.output_tokens, 8);
            assert_eq!(error.usage.reasoning_tokens, Some(3));
        }
    }
}

#[test]
fn missing_usage_details_remain_unknown_and_reported_zero_is_preserved() {
    let mut payload = completed();
    payload["usage"] = json!({"input_tokens":5,"output_tokens":2});
    let response = completed_response(payload.clone()).unwrap();
    assert_eq!(response.usage.cached_input_tokens, None);
    assert_eq!(response.usage.reasoning_tokens, None);
    payload["usage"]["input_tokens_details"] = json!({"cached_tokens":0});
    payload["usage"]["output_tokens_details"] = json!({"reasoning_tokens":0});
    let response = completed_response(payload).unwrap();
    assert_eq!(response.usage.cached_input_tokens, Some(0));
    assert_eq!(response.usage.reasoning_tokens, Some(0));
}

#[test]
fn terminal_partial_or_malformed_calls_never_become_executable() {
    for (field, value) in [
        ("call_id", Value::Null),
        ("name", json!("")),
        ("arguments", json!("{\"kind\":")),
        ("arguments", json!("[]")),
        ("status", json!("in_progress")),
    ] {
        let mut payload = completed();
        payload["output"][2][field] = value;
        assert!(completed_response(payload).is_err(), "{field}");
    }
    let mut payload = completed();
    payload["status"] = json!("incomplete");
    assert!(completed_response(payload).is_err());
    let mut payload = completed();
    let duplicate = payload["output"][2].clone();
    payload["output"].as_array_mut().unwrap().push(duplicate);
    assert!(completed_response(payload).is_err());
    assert!(completed_response(json!({"status":"completed","output":[]})).is_err());
}

#[tokio::test]
async fn named_events_separate_reasoning_and_text_and_ignore_partial_calls() {
    let mut diagnostics = ResponseDiagnostics::default();
    let (tx, mut rx) = mpsc::channel(8);
    for (kind, text) in [
        ("response.reasoning_text.delta", "thinking"),
        ("response.output_text.delta", "answer"),
    ] {
        let frame = format!("event: {kind}\r\ndata: {}", json!({"delta":text}));
        assert!(
            absorb_frame(
                frame.as_bytes(),
                Some(&tx),
                &mut ResponseDiagnostics::default()
            )
            .await
            .unwrap()
            .is_none()
        );
    }
    assert!(
        matches!(rx.recv().await.unwrap(), ProviderStreamEvent::ReasoningDelta(text) if text == "thinking")
    );
    assert!(
        matches!(rx.recv().await.unwrap(), ProviderStreamEvent::TextDelta(text) if text == "answer")
    );
    let frame = br#"data: {"type":"response.function_call_arguments.delta","delta":"{"}"#;
    assert!(
        absorb_frame(frame, Some(&tx), &mut diagnostics)
            .await
            .unwrap()
            .is_none()
    );
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn terminal_failures_and_early_done_are_errors() {
    for kind in [
        "response.failed",
        "response.incomplete",
        "response.cancelled",
        "error",
    ] {
        let frame = format!(
            "data: {}",
            json!({"type":kind,"response":{"error":{"message":"test failure"}}})
        );
        let error = absorb_frame(frame.as_bytes(), None, &mut ResponseDiagnostics::default())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("test failure"));
    }
    for frame in [
        "data: [DONE]",
        "data: {",
        "data: {}",
        "data: {\"type\":\"response.completed\"}",
    ] {
        assert!(
            absorb_frame(frame.as_bytes(), None, &mut ResponseDiagnostics::default())
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn delta_delivery_waits_for_consumer_capacity_and_reports_disconnect() {
    let mut diagnostics = ResponseDiagnostics::default();
    let (tx, mut rx) = mpsc::channel(1);
    tx.send(ProviderStreamEvent::TextDelta("first".into()))
        .await
        .unwrap();
    let frame = br#"data: {"type":"response.output_text.delta","delta":"second"}"#;
    let pending = absorb_frame(frame, Some(&tx), &mut diagnostics);
    tokio::pin!(pending);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(10), &mut pending)
            .await
            .is_err()
    );
    rx.recv().await.unwrap();
    pending.await.unwrap();
    assert!(
        matches!(rx.recv().await.unwrap(), ProviderStreamEvent::TextDelta(text) if text == "second")
    );
    drop(rx);
    let mut diagnostics = ResponseDiagnostics::default();
    assert!(
        absorb_frame(frame, Some(&tx), &mut diagnostics)
            .await
            .is_err()
    );
}
