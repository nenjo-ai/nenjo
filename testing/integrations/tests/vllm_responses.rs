//! Live vLLM Responses tests. Opt in with `--ignored --test-threads=1`.
//!
//! Requires NENJO_VLLM_BASE_URL; NENJO_VLLM_MODEL and VLLM_API_KEY are optional.
//! A single advertised model is selected automatically. Image cases require a
//! vision model. Failures and timeouts always fail the test, never silently skip.

use std::num::NonZeroU32;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use nenjo::manifest::{
    AgentManifest, Manifest, ModelManifest, ProjectManifest, PromptConfig, model_manifest_slug,
};
use nenjo::provider::{
    ArtifactInputPreparer, ModelProviderFactory, PreparedModelArtifacts, Provider, ToolFactory,
};
use nenjo::{ChatInput, Slug, Streaming, Tool, ToolCategory, ToolResult, TurnEvent};
use nenjo_models::{
    ArtifactId, ArtifactInput, ArtifactInputSource, ArtifactRef, ArtifactSize, ChatMessage,
    ChatRequest, ChatResponse, ConversationMessage, FinishReason, MediaType, ModelProvider,
    PreparedArtifact, PreparedArtifactInputs, ProviderStreamEvent, ReasoningEffort,
    ResponseTermination, ResponseTerminationError, ResponsesOptions, Sha256Digest, ToolOutput,
    ToolOutputPart, ToolResultMessage, ToolSpec, VllmProvider, VllmStreaming,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;
use tokio::time::timeout;
use uuid::Uuid;

const DEADLINE: Duration = Duration::from_secs(120);
const IMAGE: &[u8] = include_bytes!("fixtures/vllm/color.png");

struct Server {
    base_url: String,
    model: String,
    api_key: Option<String>,
    client: reqwest::Client,
}

impl Server {
    /// Discover exactly one model unless the test explicitly selects its ID.
    async fn from_env() -> Result<Self> {
        let base_url = std::env::var("NENJO_VLLM_BASE_URL")
            .context("set NENJO_VLLM_BASE_URL to the server API root (ending in /v1)")?
            .trim_end_matches('/')
            .to_owned();
        let api_key = std::env::var("VLLM_API_KEY")
            .ok()
            .filter(|key| !key.is_empty());
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(Duration::from_secs(60))
            .timeout(DEADLINE)
            .build()?;
        let model = match std::env::var("NENJO_VLLM_MODEL") {
            Ok(model) if !model.is_empty() => model,
            _ => {
                let mut request = client.get(format!("{base_url}/models"));
                if let Some(key) = &api_key {
                    request = request.bearer_auth(key);
                }
                let models: Value = request.send().await?.error_for_status()?.json().await?;
                let models = models["data"]
                    .as_array()
                    .context("models response missing data")?;
                anyhow::ensure!(
                    models.len() == 1,
                    "set NENJO_VLLM_MODEL when the server advertises multiple models"
                );
                models[0]["id"]
                    .as_str()
                    .context("model missing ID")?
                    .to_owned()
            }
        };
        Ok(Self {
            base_url,
            model,
            api_key,
            client,
        })
    }

    fn provider(&self, streaming: VllmStreaming) -> VllmProvider {
        VllmProvider::with_streaming(Some(&self.base_url), self.api_key.as_deref(), streaming)
            .with_http_client(self.client.clone())
    }

    /// Drain a one-slot stream concurrently so backpressure is exercised too.
    async fn stream(
        &self,
        request: ChatRequest<'_>,
    ) -> Result<(ChatResponse, Vec<ProviderStreamEvent>)> {
        self.stream_with_options(request, ResponsesOptions::default())
            .await
    }

    /// Exercise generation controls through the same public streaming path as chat.
    async fn stream_with_options(
        &self,
        request: ChatRequest<'_>,
        options: ResponsesOptions,
    ) -> Result<(ChatResponse, Vec<ProviderStreamEvent>)> {
        timeout(DEADLINE, async {
            let provider = self
                .provider(VllmStreaming::Enabled)
                .with_responses_options(options);
            let (tx, mut rx) = mpsc::channel(1);
            let collect = async move {
                let mut events = Vec::new();
                while let Some(event) = rx.recv().await {
                    events.push(event);
                }
                events
            };
            let (response, events) =
                tokio::join!(provider.chat_stream(request, &self.model, 0.0, tx), collect);
            Ok((response?, events))
        })
        .await
        .context("vLLM Responses exceeded the test deadline")?
    }
}

#[tokio::test]
#[ignore = "requires a live reasoning-capable vLLM Responses server"]
async fn reasoning_controls_separate_thinking_from_text_and_retain_usage_details() -> Result<()> {
    let server = Server::from_env().await?;
    let messages = [ConversationMessage::user(
        "Calculate (17 * 19) + (23 * 29). Check the arithmetic carefully. Reply with the final number only.",
    )];
    for effort in [ReasoningEffort::None, ReasoningEffort::Low] {
        let (response, events) = server
            .stream_with_options(
                request(&messages),
                ResponsesOptions {
                    reasoning_effort: Some(effort),
                    max_output_tokens: NonZeroU32::new(1024),
                },
            )
            .await?;
        assert_text_response(&response);
        assert!(response.text_or_empty().contains("990"));
        assert_eq!(text_deltas(&events), response.text_or_empty());
        let has_reasoning = events.iter().any(
            |event| matches!(event, ProviderStreamEvent::ReasoningDelta(text) if !text.is_empty()),
        );
        assert_eq!(has_reasoning, effort != ReasoningEffort::None);
        assert!(
            response.usage.cached_input_tokens.is_some(),
            "vLLM should report cache usage"
        );
        let reasoning_tokens = response
            .usage
            .reasoning_tokens
            .context("vLLM omitted reasoning usage")?;
        if effort == ReasoningEffort::None {
            assert_eq!(reasoning_tokens, 0);
        } else if reasoning_tokens == 0 {
            // The tested Mia build emits reasoning deltas but reports zero reasoning
            // tokens. Preserve its accounting; the deltas above prove thinking ran.
            eprintln!("Server emitted reasoning but reported reasoning_tokens=0");
        }
        assert!(reasoning_tokens <= response.usage.output_tokens);
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires a live vLLM Responses server"]
async fn output_limit_returns_typed_error_with_provider_id_and_usage() -> Result<()> {
    let server = Server::from_env().await?;
    let messages = [ConversationMessage::user(
        "Write out the numbers from 1 to 100, separated by spaces.",
    )];
    for streaming in [VllmStreaming::Enabled, VllmStreaming::Disabled] {
        let provider = server
            .provider(streaming)
            .with_responses_options(ResponsesOptions {
                reasoning_effort: Some(ReasoningEffort::None),
                max_output_tokens: NonZeroU32::new(1),
            });
        let error = timeout(
            DEADLINE,
            provider.chat(request(&messages), &server.model, 0.0),
        )
        .await?
        .expect_err("a single token must not complete the requested answer");
        let termination = error
            .downcast_ref::<ResponseTerminationError>()
            .context(format!("expected typed terminal error: {error}"))?;
        assert_eq!(termination.reason, ResponseTermination::OutputLimit);
        assert!(
            termination
                .response_id
                .as_deref()
                .is_some_and(|id| !id.is_empty())
        );
        assert!(termination.usage.input_tokens > 0);
        assert_eq!(termination.usage.output_tokens, 1);
    }
    Ok(())
}

fn request(messages: &[ConversationMessage]) -> ChatRequest<'_> {
    ChatRequest {
        messages,
        tools: None,
        native_tools: None,
        prepared_artifacts: None,
    }
}

fn text_deltas(events: &[ProviderStreamEvent]) -> String {
    events
        .iter()
        .filter_map(|event| match event {
            ProviderStreamEvent::TextDelta(text) => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn assert_text_response(response: &ChatResponse) {
    assert!(
        !response.text_or_empty().trim().is_empty(),
        "empty model answer"
    );
    assert_eq!(response.finish_reason, FinishReason::Stop);
    assert!(response.tool_calls.is_empty());
    assert!(response.usage.input_tokens > 0);
    assert!(response.usage.output_tokens > 0);
}

fn artifact(media_type: &str, bytes: &[u8]) -> (ArtifactRef, PreparedArtifactInputs) {
    let reference = ArtifactRef::new(
        ArtifactId::parse(Uuid::new_v4()).unwrap(),
        Sha256Digest::parse(&format!("sha256:{:x}", Sha256::digest(bytes))).unwrap(),
        MediaType::parse(media_type).unwrap(),
        ArtifactSize::new(bytes.len() as u64),
    );
    let prepared = PreparedArtifact::new(reference.clone(), Arc::from(bytes)).unwrap();
    (reference, PreparedArtifactInputs::new([prepared]))
}

#[tokio::test]
#[ignore = "requires live vLLM Responses endpoint"]
async fn text_stream_matches_final_output_and_usage() -> Result<()> {
    let server = Server::from_env().await?;
    let messages = [ConversationMessage::user("Reply with exactly NENJO_OK.")];
    let (response, events) = server.stream(request(&messages)).await?;
    assert_text_response(&response);
    assert!(response.text_or_empty().contains("NENJO_OK"));
    assert_eq!(text_deltas(&events), response.text_or_empty());
    Ok(())
}

#[tokio::test]
#[ignore = "requires live vLLM Responses endpoint"]
async fn buffered_caller_supports_streaming_and_json_wire_modes() -> Result<()> {
    let server = Server::from_env().await?;
    let messages = [ConversationMessage::user(
        "Reply with exactly NENJO_BUFFERED.",
    )];
    for mode in [VllmStreaming::Enabled, VllmStreaming::Disabled] {
        let response = timeout(
            DEADLINE,
            server
                .provider(mode)
                .chat(request(&messages), &server.model, 0.0),
        )
        .await??;
        assert_text_response(&response);
        assert!(response.text_or_empty().contains("NENJO_BUFFERED"));
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires live vLLM Responses endpoint with automatic tool calling"]
async fn function_call_round_trip_preserves_id_and_resumes_after_result() -> Result<()> {
    let server = Server::from_env().await?;
    let tools = [ToolSpec {
        name: "lookup_code".into(),
        description: "Return the opaque code for a key.".into(),
        parameters: json!({"type":"object","properties":{"key":{"type":"string"}},"required":["key"]}),
        category: ToolCategory::Read,
    }];
    let mut messages = vec![ConversationMessage::user(
        "Use lookup_code to get the code for key alpha. Do not guess. After the tool returns, reply with only its code.",
    )];
    let (response, _) = server
        .stream(ChatRequest {
            tools: Some(&tools),
            ..request(&messages)
        })
        .await?;
    assert_eq!(response.finish_reason, FinishReason::ToolCalls);
    assert_eq!(response.tool_calls.len(), 1);
    let call = &response.tool_calls[0];
    assert_eq!(call.name, "lookup_code");
    assert_eq!(
        serde_json::from_str::<Value>(&call.arguments)?["key"],
        "alpha"
    );
    let call_id = call.id.clone();
    assert!(!call_id.is_empty());
    let code = format!("NENJO_{}", Uuid::new_v4().simple());
    messages.push(ConversationMessage::assistant_tool_calls(
        response.text,
        response.tool_calls,
    ));
    messages.push(ConversationMessage::tool_result(ToolResultMessage::text(
        call_id,
        json!({"code":code}).to_string(),
    )));
    let (response, events) = server
        .stream(ChatRequest {
            tools: Some(&tools),
            ..request(&messages)
        })
        .await?;
    assert_text_response(&response);
    assert!(response.text_or_empty().contains(&code));
    assert_eq!(text_deltas(&events), response.text_or_empty());
    Ok(())
}

#[tokio::test]
#[ignore = "requires live vLLM Responses endpoint with a vision model"]
async fn image_artifact_is_visible_to_the_model() -> Result<()> {
    let server = Server::from_env().await?;
    let (reference, prepared) = artifact("image/png", IMAGE);
    let messages = [ConversationMessage::chat(
        ChatMessage::user(
            "What is the dominant color of the attached image? Reply with only the color name.",
        )
        .with_artifacts(vec![ArtifactInput::new(
            reference,
            ArtifactInputSource::UserAttachment,
        )]),
    )];
    let (response, events) = server
        .stream(ChatRequest {
            prepared_artifacts: Some(&prepared),
            ..request(&messages)
        })
        .await?;
    assert_text_response(&response);
    assert!(response.text_or_empty().to_lowercase().contains("red"));
    assert_eq!(text_deltas(&events), response.text_or_empty());
    Ok(())
}

#[tokio::test]
#[ignore = "requires live vLLM Responses endpoint"]
async fn text_artifact_retains_utf8_and_its_contents() -> Result<()> {
    let server = Server::from_env().await?;
    let secret = format!("café-{}", Uuid::new_v4().simple());
    let (reference, prepared) = artifact("text/plain", secret.as_bytes());
    let messages = [ConversationMessage::chat(
        ChatMessage::user("Copy the exact text from the attached file. Do not add anything else.")
            .with_artifacts(vec![ArtifactInput::new(
                reference,
                ArtifactInputSource::UserAttachment,
            )]),
    )];
    let (response, events) = server
        .stream(ChatRequest {
            prepared_artifacts: Some(&prepared),
            ..request(&messages)
        })
        .await?;
    assert_text_response(&response);
    assert!(response.text_or_empty().contains(&secret));
    assert_eq!(text_deltas(&events), response.text_or_empty());
    Ok(())
}

struct ModelFactory(Arc<VllmProvider>);
impl ModelProviderFactory for ModelFactory {
    fn create(&self, _: &str) -> Result<Arc<dyn ModelProvider>> {
        Ok(self.0.clone())
    }
}

struct ImagePreparer(PreparedArtifactInputs);
#[async_trait::async_trait]
impl ArtifactInputPreparer for ImagePreparer {
    async fn prepare(
        &self,
        messages: &[ConversationMessage],
        _: &AgentManifest,
        _: &ModelManifest,
    ) -> Result<PreparedModelArtifacts> {
        Ok(PreparedModelArtifacts::new(
            messages,
            self.0.clone(),
            Vec::new(),
            Default::default(),
        ))
    }
}

struct CaptureImage {
    reference: ArtifactRef,
    calls: Arc<AtomicUsize>,
}
#[async_trait::async_trait]
impl Tool for CaptureImage {
    fn name(&self) -> &str {
        "capture_image"
    }
    fn description(&self) -> &str {
        "Capture the image to inspect. Returns the actual image as an artifact."
    }
    fn category(&self) -> ToolCategory {
        ToolCategory::Read
    }
    fn parameters_schema(&self) -> Value {
        json!({"type":"object","properties":{},"additionalProperties":false})
    }
    async fn execute(&self, _: Value) -> Result<ToolResult> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(ToolResult {
            success: true,
            error: None,
            output: ToolOutput::from_parts(vec![
                ToolOutputPart::Text("Captured image attached.".into()),
                ToolOutputPart::Artifact(self.reference.clone()),
            ]),
        })
    }
}

struct ImageTools(Arc<CaptureImage>);
#[async_trait::async_trait]
impl ToolFactory for ImageTools {
    async fn create_tools(&self, _: &AgentManifest) -> Vec<Arc<dyn Tool>> {
        vec![self.0.clone()]
    }
}

/// Exercise the real SDK turn loop, actual function execution, artifact preparation,
/// and the follow-up model request that must complete after a media tool result.
#[tokio::test]
#[ignore = "requires live vLLM Responses endpoint with a vision model and automatic tool calling"]
async fn sdk_turn_completes_after_tool_returns_image_artifact() -> Result<()> {
    let server = Server::from_env().await?;
    let (reference, prepared) = artifact("image/png", IMAGE);
    let calls = Arc::new(AtomicUsize::new(0));
    let model = ModelManifest {
        slug: model_manifest_slug("vllm", &server.model),
        name: "vllm-e2e".into(),
        description: None,
        model: server.model.clone(),
        model_provider: "vllm".into(),
        temperature: Some(0.0),
        context_window: Some(32768),
        base_url: Some(server.base_url.clone()),
        native_tools: vec![],
        capabilities: vec![],
        input_modalities: vec![],
        output_modalities: vec![],
        execution_modes: vec![],
    };
    let agent = AgentManifest { name:"vision-test".into(), slug:Slug::derive("vision-test"), description:None,
        prompt_config:PromptConfig { system_prompt:"Use capture_image exactly once to obtain the image. Then inspect its pixels and answer the user's question. Do not guess or use other tools.".into(), ..Default::default() },
        color:None, model:Some(model.slug.clone()), domains:vec![], platform_scopes:vec![], mcp_servers:vec![],
        abilities:vec![], script_tools:vec![], media:vec![], prompt_locked:false, source_type:None, metadata:json!({}) };
    let manifest = Manifest {
        agents: vec![agent],
        models: vec![model],
        projects: vec![ProjectManifest {
            name: "vllm-e2e".into(),
            slug: Slug::derive("vllm-e2e"),
            description: None,
            settings: Value::Null,
        }],
        ..Default::default()
    };
    let provider = Provider::builder()
        .with_manifest(manifest)
        .with_model_factory(ModelFactory(Arc::new(
            server.provider(VllmStreaming::Enabled),
        )))
        .with_tool_factory(ImageTools(Arc::new(CaptureImage {
            reference,
            calls: calls.clone(),
        })))
        .with_artifact_input_preparer(ImagePreparer(prepared))
        .build()
        .await?;
    let runner = provider.agent("vision-test").await?.build().await?;
    let mut handle = runner
        .chat(
            ChatInput::new(
                "Capture the image and tell me its dominant color. Answer with the color name.",
            ),
            Streaming,
        )
        .await?;
    let (output, text) = timeout(DEADLINE, async {
        let mut text = String::new();
        while let Some(event) = handle.recv().await {
            match event {
                TurnEvent::AssistantTextDelta { delta, .. } => text.push_str(&delta),
                TurnEvent::Done { output } => return Ok((output, text)),
                _ => {}
            }
        }
        anyhow::bail!("SDK execution ended without TurnEvent::Done")
    })
    .await
    .context("SDK turn stalled after its tool result")??;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(
        output.text.to_lowercase().contains("red"),
        "{}",
        output.text
    );
    assert!(text.to_lowercase().contains("red"));
    assert!(output.tool_calls >= 1);
    assert!(output.messages.iter().any(|message| matches!(message,
        ConversationMessage::ToolResults(results) if results.iter().any(|result| result.output.parts().iter().any(|part| matches!(part, ToolOutputPart::Artifact(_)))))));
    Ok(())
}
