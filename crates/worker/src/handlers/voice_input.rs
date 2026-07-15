//! Push-to-talk voice input handlers.

use anyhow::{Context, Result};
use nenjo_events::{Response, VoiceInputAudio, VoiceTranscriptSegment};
use nenjo_models::{
    MediaInputAsset, NativeMediaRequest, NativeMediaResponse, TranscribeAudioRequest,
};
use serde_json::json;
use uuid::Uuid;

use crate::crypto::{ContentScope, encrypt_text_with_provider};
use crate::runtime::CommandContext;

const CLIENT_TRANSCRIPTION_PROMPT: &str = "Transcribe the attached audio accurately in its original language. Return only the transcript with natural punctuation and capitalization. Do not translate, summarize, answer the content, infer missing words, add speaker labels, timestamps, Markdown, or commentary.";

pub(crate) struct VoiceInputTranscribeRequest<'a> {
    pub job_id: Uuid,
    pub session_id: Uuid,
    pub audio: VoiceInputAudio,
    pub provider: &'a str,
    pub model: &'a str,
    pub base_url: Option<&'a str>,
    pub language: Option<&'a str>,
}

pub(crate) async fn handle_voice_input_transcribe(
    ctx: &CommandContext,
    request: VoiceInputTranscribeRequest<'_>,
) -> Result<()> {
    let response = transcribe(ctx, &request).await;
    match response {
        Ok(response) => ctx.response_tx.send(response)?,
        Err(error) => {
            ctx.response_tx.send(Response::VoiceInputFailed {
                job_id: request.job_id,
                session_id: request.session_id,
                error_code: "transcription_failed".to_string(),
                error_message: error_chain_message(&error),
            })?;
        }
    }
    Ok(())
}

fn error_chain_message(error: &anyhow::Error) -> String {
    let mut messages = Vec::new();
    for cause in error.chain() {
        let message = cause.to_string();
        if messages.last() != Some(&message) {
            messages.push(message);
        }
    }
    messages.join(": ")
}

async fn transcribe(
    ctx: &CommandContext,
    request: &VoiceInputTranscribeRequest<'_>,
) -> Result<Response> {
    let provider_name = request.provider.trim();
    if provider_name.is_empty() {
        anyhow::bail!("voice input provider cannot be empty");
    }
    let model = request.model.trim();
    if model.is_empty() {
        anyhow::bail!("voice input model cannot be empty");
    }

    let provider = ctx
        .provider_registry
        .provider_with_base_url(provider_name, request.base_url)
        .with_context(|| format!("failed to initialize media provider '{provider_name}'"))?;

    let mut provider_options = json!({
        "response_format": "verbose_json",
    });
    if let Some(object_key) = request.audio.object_key.as_deref() {
        provider_options["source_object_key"] = json!(object_key);
    }

    let response = provider
        .submit_media(NativeMediaRequest::TranscribeAudio(
            TranscribeAudioRequest {
                model: model.to_string(),
                audio: MediaInputAsset::DataUri {
                    data_uri: request.audio.data_uri.clone(),
                },
                language: request
                    .language
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string),
                prompt: Some(CLIENT_TRANSCRIPTION_PROMPT.to_string()),
                provider_options,
            },
        ))
        .await
        .with_context(|| format!("{provider_name} transcription failed for model {model}"))?;

    let NativeMediaResponse::Transcript {
        text,
        language,
        duration_seconds,
        segments,
        metadata,
    } = response
    else {
        anyhow::bail!("transcription provider returned a non-transcript response");
    };

    if text.trim().is_empty() {
        anyhow::bail!("transcription provider returned an empty transcript");
    }

    let encrypted_transcript = encrypt_text_with_provider(
        &ctx.auth_provider,
        ContentScope::User,
        ctx.actor_user_id,
        Uuid::new_v4(),
        "voice_input.transcript",
        text.trim(),
    )
    .await
    .context("failed to encrypt transcript for user")?;

    Ok(Response::VoiceInputTranscribed {
        job_id: request.job_id,
        session_id: request.session_id,
        encrypted_transcript,
        language,
        duration_seconds,
        segments: segments
            .into_iter()
            .map(|segment| VoiceTranscriptSegment {
                start_seconds: segment.start_seconds,
                end_seconds: segment.end_seconds,
                text: segment.text,
                metadata: segment.metadata,
            })
            .collect(),
        provider: provider_name.to_string(),
        model: model.to_string(),
        metadata,
    })
}
