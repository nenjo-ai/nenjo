//! Explicit Responses transport sharing compatible endpoint, auth, and HTTP policy.

use std::num::NonZeroU32;

use serde::Serialize;
use tokio::sync::mpsc::Sender;
use tracing::debug;

use super::OpenAiCompatibleProvider;
use crate::openai_chat::InstructionRolePolicy;
use crate::openai_responses::diagnostics::ResponseDiagnostics;
use crate::openai_responses::{
    ResponsesInputItem, ResponsesTool, convert_input_with_artifacts, convert_local_tools, stream,
};
use crate::{
    ChatRequest, ChatResponse, ModelProvider, ProviderStreamEvent, ReasoningEffort,
    ResponsesOptions,
};

#[derive(Serialize)]
struct Reasoning {
    effort: ReasoningEffort,
}

#[derive(Serialize)]
struct ResponsesRequest {
    model: String,
    input: Vec<ResponsesInputItem>,
    tools: Vec<ResponsesTool>,
    tool_choice: &'static str,
    parallel_tool_calls: bool,
    temperature: f64,
    stream: bool,
    store: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning: Option<Reasoning>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_output_tokens: Option<NonZeroU32>,
}

impl OpenAiCompatibleProvider {
    /// Send full local history without relying on provider-side response storage.
    /// Endpoint errors are returned directly; generation is never retried on a sibling API.
    #[tracing::instrument(target = "nenjo_models::responses", level = "debug", skip_all, fields(provider = %self.name, model, api = "responses"))]
    pub(crate) async fn chat_responses(
        &self,
        request: ChatRequest<'_>,
        model: &str,
        temperature: f64,
        streaming: bool,
        events: Option<&Sender<ProviderStreamEvent>>,
        options: ResponsesOptions,
    ) -> anyhow::Result<ChatResponse> {
        anyhow::ensure!(
            request.native_tools.is_none_or(|tools| tools.is_empty()),
            "vLLM Responses does not support configured provider-native tools"
        );
        let native_request = ResponsesRequest {
            model: model.to_owned(),
            input: convert_input_with_artifacts(
                &request,
                InstructionRolePolicy::from_supports_developer_role(
                    self.supports_developer_role(model),
                ),
            )?,
            tools: convert_local_tools(request.tools)?,
            tool_choice: "auto",
            parallel_tool_calls: true,
            temperature,
            stream: streaming,
            store: false,
            reasoning: options.reasoning_effort.map(|effort| Reasoning { effort }),
            max_output_tokens: options.max_output_tokens,
        };
        crate::request_logging::debug_provider_request(
            &self.name,
            model,
            1,
            request.messages,
            &native_request,
        );
        let url = self.responses_url();
        debug!(
            target: "nenjo_models::responses",
            provider = self.name,
            model,
            streaming,
            reasoning_effort = options
                .reasoning_effort
                .map(ReasoningEffort::as_str)
                .unwrap_or("default"),
            max_output_tokens = options.max_output_tokens.map(NonZeroU32::get),
            "Sending Responses API request"
        );
        let mut diagnostics = ResponseDiagnostics::default();
        let result = async {
            let pending = self
                .apply_auth_header(
                    self.client.post(&url).json(&native_request),
                    self.api_key.as_deref().unwrap_or(""),
                )
                .send();
            let response = if let Some(events) = events {
                tokio::select! {
                    biased;
                    () = events.closed() => anyhow::bail!("provider stream consumer closed"),
                    response = pending => response?,
                }
            } else {
                pending.await?
            };
            debug!(
                target: "nenjo_models::responses",
                provider_request_id = response
                    .headers()
                    .get("x-request-id")
                    .and_then(|id| id.to_str().ok()),
                http_status = response.status().as_u16(),
                "Responses HTTP headers received"
            );
            if !response.status().is_success() {
                let status = response.status();
                let error = response.text().await?;
                anyhow::bail!(
                    "{} Responses API request to {url} failed with {status}: {error}",
                    self.name
                );
            }
            if streaming {
                stream::read_stream(response, events, &mut diagnostics).await
            } else {
                let pending = response.json();
                let value = if let Some(events) = events {
                    tokio::select! {
                        biased;
                        () = events.closed() => anyhow::bail!("provider stream consumer closed"),
                        value = pending => value?,
                    }
                } else {
                    pending.await?
                };
                diagnostics.observe(&value);
                stream::completed_response(value)
            }
        }
        .await;
        diagnostics.finish(&result);
        result
    }
}
