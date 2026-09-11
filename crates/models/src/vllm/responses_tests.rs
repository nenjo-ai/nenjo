//! HTTP contract tests for the selectable vLLM Responses transport.

use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio::time::timeout;

use super::tests::prepared_artifact;
use super::*;
use crate::{
    ArtifactInput, ArtifactInputSource, ChatMessage, ConversationMessage, PreparedArtifactInputs,
    ReasoningEffort, ToolResultMessage,
};

/// Leave the HTTP body open until the caller releases it, independently of SSE completion.
async fn endpoint(
    body: String,
    streaming: bool,
    status: u16,
) -> (
    String,
    oneshot::Receiver<(String, Value)>,
    oneshot::Sender<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (captured_tx, captured) = oneshot::channel();
    let (close, closed) = oneshot::channel();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut chunk = [0; 4096];
        let headers_end = loop {
            let read = socket.read(&mut chunk).await.unwrap();
            assert_ne!(read, 0);
            request.extend_from_slice(&chunk[..read]);
            if let Some(index) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                break index + 4;
            }
        };
        let headers = String::from_utf8(request[..headers_end].to_vec()).unwrap();
        let len = headers
            .lines()
            .find_map(|line| {
                let (key, value) = line.split_once(':')?;
                key.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().unwrap())
            })
            .unwrap();
        while request.len() < headers_end + len {
            let read = socket.read(&mut chunk).await.unwrap();
            assert_ne!(read, 0);
            request.extend_from_slice(&chunk[..read]);
        }
        let _ = captured_tx.send((
            headers,
            serde_json::from_slice(&request[headers_end..headers_end + len]).unwrap(),
        ));
        let content_type = if streaming {
            "text/event-stream"
        } else {
            "application/json"
        };
        socket.write_all(format!("HTTP/1.1 {status} Test\r\ncontent-type: {content_type}\r\ntransfer-encoding: chunked\r\n\r\n").as_bytes()).await.unwrap();
        // Split UTF-8 and frame boundaries across transport chunks.
        for bytes in body.as_bytes().chunks(7) {
            if socket
                .write_all(format!("{:x}\r\n", bytes.len()).as_bytes())
                .await
                .is_err()
            {
                return;
            }
            if socket.write_all(bytes).await.is_err() {
                return;
            }
            if socket.write_all(b"\r\n").await.is_err() {
                return;
            }
        }
        if streaming {
            let _ = closed.await;
        }
        let _ = socket.write_all(b"0\r\n\r\n").await;
    });
    (format!("http://{address}/custom/v1"), captured, close)
}

fn response() -> Value {
    json!({"status":"completed","output":[{"type":"message","content":[{"type":"output_text","text":"café 🌍"}]}],"usage":{"input_tokens":17,"output_tokens":4}})
}

fn frames() -> String {
    format!(
        "event: response.output_text.delta\r\ndata: {}\r\n\r\nevent: response.completed\r\ndata: {}\r\n\r\n",
        json!({"delta":"café 🌍"}),
        json!({"response":response()})
    )
}

fn request(messages: &[ConversationMessage]) -> ChatRequest<'_> {
    ChatRequest {
        messages,
        tools: None,
        native_tools: None,
        prepared_artifacts: None,
    }
}

#[tokio::test]
async fn default_api_completes_both_public_delivery_modes_before_http_eof() {
    for streaming_caller in [false, true] {
        let (url, captured, _close) = endpoint(frames(), true, 200).await;
        let provider = VllmProvider::new(Some(&url), None);
        let messages = [ConversationMessage::user("hello")];
        let (tx, mut rx) = mpsc::channel(8);
        let response = timeout(Duration::from_secs(2), async {
            if streaming_caller {
                provider
                    .chat_stream(request(&messages), "test", 0.0, tx)
                    .await
            } else {
                provider.chat(request(&messages), "test", 0.0).await
            }
        })
        .await
        .expect("must finish without HTTP EOF")
        .unwrap();
        let (headers, body) = captured.await.unwrap();
        assert!(headers.starts_with("POST /custom/v1/responses HTTP/1.1"));
        assert!(!headers.to_lowercase().contains("authorization:"));
        assert_eq!(body["stream"], true);
        assert_eq!(body["store"], false);
        assert_eq!(body["tool_choice"], "auto");
        assert_eq!(body["parallel_tool_calls"], true);
        assert!(body.get("previous_response_id").is_none());
        assert!(body.get("messages").is_none());
        assert_eq!(body["reasoning"]["effort"], "max");
        assert!(body.get("max_output_tokens").is_none());
        assert_eq!(response.text.as_deref(), Some("café 🌍"));
        assert_eq!(response.usage.input_tokens, 17);
        assert_eq!(response.usage.output_tokens, 4);
        assert_eq!(response.finish_reason, crate::FinishReason::Stop);
        if streaming_caller {
            assert!(
                matches!(rx.recv().await.unwrap(), ProviderStreamEvent::TextDelta(text) if text == "café 🌍")
            );
        }
    }
}

#[tokio::test]
async fn generation_controls_are_identical_for_streaming_and_json_wire_modes() {
    for streaming in [true, false] {
        for effort in [
            None,
            Some(ReasoningEffort::None),
            Some(ReasoningEffort::Low),
            Some(ReasoningEffort::High),
            Some(ReasoningEffort::Max),
        ] {
            let body = if streaming {
                frames()
            } else {
                response().to_string()
            };
            let (url, captured, _close) = endpoint(body, streaming, 200).await;
            let provider = VllmProvider::with_streaming(Some(&url), None, streaming.into())
                .with_api(VllmApi::Responses)
                .with_responses_options(ResponsesOptions {
                    reasoning_effort: effort,
                    max_output_tokens: NonZeroU32::new(123),
                });
            provider
                .chat(request(&[ConversationMessage::user("hello")]), "test", 0.0)
                .await
                .unwrap();
            let (_, body) = captured.await.unwrap();
            assert_eq!(
                body["reasoning"]["effort"],
                effort.unwrap_or(ReasoningEffort::Max).as_str()
            );
            assert_eq!(body["max_output_tokens"], 123);
            assert_eq!(body["tool_choice"], "auto");
            assert_eq!(body["parallel_tool_calls"], true);
            assert!(
                body.get("include_reasoning").is_none(),
                "hiding reasoning is not disabling generation"
            );
        }
    }
}

#[tokio::test]
async fn responses_controls_cannot_be_silently_ignored_by_chat_completions() {
    let provider = VllmProvider::new(Some("http://127.0.0.1:1/v1"), None)
        .with_api(VllmApi::ChatCompletions)
        .with_responses_options(ResponsesOptions {
            reasoning_effort: Some(ReasoningEffort::None),
            ..Default::default()
        });
    let messages = [ConversationMessage::user("hello")];
    let error = provider
        .chat(request(&messages), "test", 0.0)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("require api = responses"));
}

#[tokio::test]
async fn default_api_uses_responses_json_and_bearer_auth_when_streaming_is_disabled() {
    let (url, captured, _close) = endpoint(response().to_string(), false, 200).await;
    let provider =
        VllmProvider::with_streaming(Some(&url), Some("test-token"), VllmStreaming::Disabled);
    let messages = [ConversationMessage::user("hello")];
    let response = provider
        .chat(request(&messages), "test", 0.0)
        .await
        .unwrap();
    let (headers, body) = captured.await.unwrap();
    assert!(
        headers
            .to_lowercase()
            .contains("authorization: bearer test-token")
    );
    assert_eq!(body["stream"], false);
    assert_eq!(body["reasoning"]["effort"], "max");
    assert_eq!(body["tool_choice"], "auto");
    assert_eq!(body["parallel_tool_calls"], true);
    assert_eq!(response.text.as_deref(), Some("café 🌍"));
}

#[tokio::test]
async fn truncated_stream_and_http_errors_do_not_fall_back_or_succeed() {
    for (body, streaming, status) in [
        (
            "data: {\"type\":\"response.function_call_arguments.delta\",\"delta\":\"{\"}\n\n",
            true,
            200,
        ),
        ("Responses unavailable", false, 404),
    ] {
        let (url, _captured, close) = endpoint(body.into(), streaming, status).await;
        let _ = close.send(());
        let provider = VllmProvider::new(Some(&url), None).with_api(VllmApi::Responses);
        let messages = [ConversationMessage::user("hello")];
        let error = timeout(
            Duration::from_secs(2),
            provider.chat(request(&messages), "test", 0.0),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert!(error.to_string().contains(if streaming {
            "before response.completed"
        } else {
            "404"
        }));
    }
}

#[tokio::test]
async fn closing_consumer_cancels_a_quiet_open_http_stream() {
    let (url, captured, _close) = endpoint(String::new(), true, 200).await;
    let provider = Arc::new(VllmProvider::new(Some(&url), None).with_api(VllmApi::Responses));
    let (tx, rx) = mpsc::channel(1);
    let call = tokio::spawn(async move {
        let messages = [ConversationMessage::user("hello")];
        provider
            .chat_stream(request(&messages), "test", 0.0, tx)
            .await
    });
    captured.await.unwrap();
    drop(rx);
    let error = timeout(Duration::from_secs(2), call)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(error.to_string().contains("consumer closed"));
}

#[tokio::test]
async fn attachments_and_tool_artifacts_are_encoded_without_losing_call_ids() {
    let (url, captured, _close) = endpoint(frames(), true, 200).await;
    let provider = VllmProvider::new(Some(&url), None).with_api(VllmApi::Responses);
    let (image, prepared) = prepared_artifact("image/png", b"png");
    let messages = [
        ConversationMessage::developer("instruction"),
        ConversationMessage::chat(ChatMessage::user("describe").with_artifacts(vec![
            ArtifactInput::new(image.clone(), ArtifactInputSource::UserAttachment),
        ])),
        ConversationMessage::assistant_tool_calls(
            None,
            vec![crate::ToolCall {
                id: "call-1".into(),
                name: "capture".into(),
                arguments: "{}".into(),
            }],
        ),
        ConversationMessage::tool_result(
            ToolResultMessage::text("call-1", "captured").with_artifact(image),
        ),
    ];
    provider
        .chat(
            ChatRequest {
                prepared_artifacts: Some(&prepared),
                ..request(&messages)
            },
            "glm-test",
            0.0,
        )
        .await
        .unwrap();
    let body = captured.await.unwrap().1;
    assert_eq!(body["input"][0]["role"], "user");
    assert_eq!(body["input"][0]["content"][0]["text"], "instruction");
    assert_eq!(body["input"][0]["content"][3]["type"], "input_image");
    assert_eq!(body["input"][2]["call_id"], "call-1");
    assert_eq!(
        body["input"][2]["output"][1]["image_url"],
        "data:image/png;base64,cG5n"
    );
}

#[tokio::test]
async fn unsupported_media_and_missing_preparation_fail_before_network() {
    let provider = VllmProvider::new(Some("http://127.0.0.1:9"), None).with_api(VllmApi::Responses);
    for media_type in ["application/pdf", "audio/wav", "video/mp4"] {
        let (reference, prepared) = prepared_artifact(media_type, b"media");
        assert_eq!(
            provider.artifact_input_transport(
                "test",
                ModelCapabilityId::Chat,
                reference.media_type()
            ),
            ArtifactInputTransport::Unsupported
        );
        let messages = [ConversationMessage::chat(
            ChatMessage::user("read").with_artifacts(vec![ArtifactInput::new(
                reference,
                ArtifactInputSource::UserAttachment,
            )]),
        )];
        let error = provider
            .chat(
                ChatRequest {
                    prepared_artifacts: Some(&prepared),
                    ..request(&messages)
                },
                "test",
                0.0,
            )
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("unsupported Responses media type")
        );
        let error = provider
            .chat(
                ChatRequest {
                    prepared_artifacts: Some(&PreparedArtifactInputs::default()),
                    ..request(&messages)
                },
                "test",
                0.0,
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("unresolved artifact"));
    }
}
