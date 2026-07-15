//! Direct provider media tools.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;
use nenjo::tools::AsyncOperationKind;
use nenjo::{AsyncOperationHandle, StartAsyncOperation, current_async_operation_runtime};
use nenjo_models::{
    EditImageRequest, EditVideoRequest, ExtendVideoRequest, GenerateImageRequest,
    GenerateSpeechRequest, GenerateVideoRequest, ImageToVideoRequest, MediaOperation,
    MediaOutputAsset, MediaToolSpec, NativeMediaJob, NativeMediaJobStatus, NativeMediaRequest,
    NativeMediaResponse, ReferenceToVideoRequest, TranscribeAudioRequest,
};
use serde_json::json;

use crate::media::ResolvedMediaProvider;
use crate::providers::ModelProviderRegistry;
use crate::tools::{Tool, ToolCategory, ToolResult};

static MEDIA_OPERATION_COUNTER: AtomicU64 = AtomicU64::new(1);
const MEDIA_JOB_POLL_INTERVAL: Duration = Duration::from_secs(2);

pub struct NativeMediaTool {
    resolved: ResolvedMediaProvider,
    spec: MediaToolSpec,
    provider_registry: Arc<ModelProviderRegistry>,
}

impl NativeMediaTool {
    pub fn new(
        resolved: ResolvedMediaProvider,
        provider_registry: Arc<ModelProviderRegistry>,
    ) -> Option<Self> {
        let spec = resolved_tool_spec(&resolved, &provider_registry)?;
        Some(Self {
            resolved,
            spec,
            provider_registry,
        })
    }

    fn request(&self, mut args: serde_json::Value) -> Result<NativeMediaRequest> {
        inject_model(&mut args, &self.resolved.model);
        match self.resolved.capability {
            MediaOperation::GenerateImage => Ok(NativeMediaRequest::GenerateImage(
                serde_json::from_value::<GenerateImageRequest>(args)
                    .context("invalid generate_image request")?,
            )),
            MediaOperation::EditImage => Ok(NativeMediaRequest::EditImage(
                serde_json::from_value::<EditImageRequest>(args)
                    .context("invalid edit_image request")?,
            )),
            MediaOperation::GenerateVideo => Ok(NativeMediaRequest::GenerateVideo(
                serde_json::from_value::<GenerateVideoRequest>(args)
                    .context("invalid generate_video request")?,
            )),
            MediaOperation::EditVideo => Ok(NativeMediaRequest::EditVideo(
                serde_json::from_value::<EditVideoRequest>(args)
                    .context("invalid edit_video request")?,
            )),
            MediaOperation::ImageToVideo => Ok(NativeMediaRequest::ImageToVideo(
                serde_json::from_value::<ImageToVideoRequest>(args)
                    .context("invalid image_to_video request")?,
            )),
            MediaOperation::ReferenceToVideo => Ok(NativeMediaRequest::ReferenceToVideo(
                serde_json::from_value::<ReferenceToVideoRequest>(args)
                    .context("invalid reference_to_video request")?,
            )),
            MediaOperation::ExtendVideo => Ok(NativeMediaRequest::ExtendVideo(
                serde_json::from_value::<ExtendVideoRequest>(args)
                    .context("invalid extend_video request")?,
            )),
            MediaOperation::GenerateSpeech => Ok(NativeMediaRequest::GenerateSpeech(
                serde_json::from_value::<GenerateSpeechRequest>(args)
                    .context("invalid generate_speech request")?,
            )),
            MediaOperation::TranscribeAudio => Ok(NativeMediaRequest::TranscribeAudio(
                serde_json::from_value::<TranscribeAudioRequest>(args)
                    .context("invalid transcribe_audio request")?,
            )),
            MediaOperation::RealtimeVoiceAgent => {
                anyhow::bail!("realtime_voice_agent does not have a worker media tool yet")
            }
        }
    }
}

#[async_trait]
impl Tool for NativeMediaTool {
    fn category(&self) -> ToolCategory {
        ToolCategory::Write
    }

    fn name(&self) -> &str {
        self.spec.tool_name.as_str()
    }

    fn description(&self) -> &str {
        self.spec.description.as_str()
    }

    fn parameters_schema(&self) -> serde_json::Value {
        self.spec.parameters_schema.clone()
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let request = self.request(args)?;
        let operation = request.operation();
        let provider = self
            .provider_registry
            .provider_with_base_url(&self.resolved.provider, self.resolved.base_url.as_deref())
            .with_context(|| {
                format!(
                    "failed to initialize media provider '{}'",
                    self.resolved.provider
                )
            })?;
        let response = provider.submit_media(request).await.with_context(|| {
            format!(
                "{} media operation failed for model {}",
                self.resolved.provider, self.resolved.model
            )
        })?;

        match response {
            NativeMediaResponse::Job { job } => {
                if let Some(runtime) = current_async_operation_runtime() {
                    let operation_name = tool_name(operation).unwrap_or("media");
                    let sequence = MEDIA_OPERATION_COUNTER.fetch_add(1, Ordering::Relaxed);
                    let operation_id = format!("media_{operation_name}_{sequence}");
                    let handle = runtime
                        .start(StartAsyncOperation {
                            id: operation_id.clone(),
                            kind: AsyncOperationKind::Media,
                            label: format!("{} {operation_name}", self.resolved.provider),
                            parent_operation_id: None,
                            parent_tool_name: Some(self.name().to_string()),
                            started_summary: format!(
                                "Started {} media job {}",
                                self.resolved.provider, job.job_id
                            ),
                            model_visible: true,
                        })
                        .await;
                    let join = tokio::spawn(poll_media_job(
                        handle.clone(),
                        provider.clone(),
                        job.clone(),
                    ));
                    handle.attach_join(join).await;

                    return Ok(ToolResult {
                        success: true,
                        output: serde_json::to_string_pretty(&json!({
                            "type": "job_started",
                            "operation_id": operation_id,
                            "operation": operation,
                            "provider": &self.resolved.provider,
                            "model": &self.resolved.model,
                            "provider_job": job,
                            "next_step": {
                                "tool": "wait_operations",
                                "kind": "media",
                                "instruction": "This media request is still rendering asynchronously. Do not call this generation tool again for the same user request. Use wait_operations with kind=media until the operation completes or fails, then inspect_operations if you need the final output payload."
                            }
                        }))?,
                        error: None,
                    });
                }

                Ok(ToolResult {
                    success: true,
                    output: serde_json::to_string_pretty(&NativeMediaResponse::Job { job })?,
                    error: None,
                })
            }
            response => Ok(ToolResult {
                success: true,
                output: serde_json::to_string_pretty(&response)?,
                error: None,
            }),
        }
    }
}

fn resolved_tool_spec(
    resolved: &ResolvedMediaProvider,
    provider_registry: &ModelProviderRegistry,
) -> Option<MediaToolSpec> {
    let capabilities = provider_registry.media_capabilities(&resolved.provider)?;
    capabilities
        .models
        .into_iter()
        .filter(|model| model_pattern_matches(&model.model_pattern, &resolved.model))
        .flat_map(|model| model.tools)
        .find(|tool| tool.capability == resolved.capability)
}

async fn poll_media_job(
    operation: AsyncOperationHandle,
    provider: Arc<dyn nenjo_models::ModelProvider>,
    mut job: NativeMediaJob,
) {
    let cancel = operation.cancel_token();
    loop {
        match job.status {
            NativeMediaJobStatus::Completed => {
                operation
                    .complete(
                        format!("Media job {} completed", job.job_id),
                        response_value(&NativeMediaResponse::Job { job }),
                    )
                    .await;
                return;
            }
            NativeMediaJobStatus::Failed
            | NativeMediaJobStatus::Expired
            | NativeMediaJobStatus::Cancelled => {
                operation
                    .fail(format!(
                        "Media job {} ended with status {:?}",
                        job.job_id, job.status
                    ))
                    .await;
                return;
            }
            NativeMediaJobStatus::Queued | NativeMediaJobStatus::Running => {
                operation
                    .progress(
                        format!("Media job {} is {:?}", job.job_id, job.status),
                        serde_json::to_string(&job).ok(),
                    )
                    .await;
            }
        }

        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = tokio::time::sleep(MEDIA_JOB_POLL_INTERVAL) => {}
        }

        match provider.poll_media_job(&job).await {
            Ok(NativeMediaResponse::Assets { assets, metadata }) => {
                let summary = completed_assets_summary(&job, &assets);
                operation
                    .complete(
                        summary,
                        response_value(&NativeMediaResponse::Assets { assets, metadata }),
                    )
                    .await;
                return;
            }
            Ok(response @ NativeMediaResponse::Transcript { .. }) => {
                operation
                    .complete(
                        format!("Media job {} completed with transcript", job.job_id),
                        response_value(&response),
                    )
                    .await;
                return;
            }
            Ok(NativeMediaResponse::Job { job: next }) => {
                job = next;
            }
            Err(error) => {
                operation
                    .fail(format!("Media job {} polling failed: {error}", job.job_id))
                    .await;
                return;
            }
        }
    }
}

fn response_value(response: &NativeMediaResponse) -> Option<serde_json::Value> {
    serde_json::to_value(response).ok()
}

fn completed_assets_summary(job: &NativeMediaJob, assets: &[MediaOutputAsset]) -> String {
    match assets.first() {
        Some(MediaOutputAsset::Url { url, .. }) => {
            format!("Media job {} completed. URL: {url}", job.job_id)
        }
        Some(MediaOutputAsset::ProviderFileId { file_id, .. }) => {
            format!(
                "Media job {} completed. Provider file id: {file_id}",
                job.job_id
            )
        }
        Some(MediaOutputAsset::Base64 { .. }) => {
            format!(
                "Media job {} completed with base64 media output",
                job.job_id
            )
        }
        None => format!("Media job {} completed", job.job_id),
    }
}

fn inject_model(args: &mut serde_json::Value, model: &str) {
    if !args.is_object() {
        *args = json!({});
    }
    let object = args.as_object_mut().expect("object initialized");
    object.insert(
        "model".to_string(),
        serde_json::Value::String(model.to_string()),
    );
}

pub fn tool_name(operation: MediaOperation) -> Option<&'static str> {
    operation.tool_name()
}

fn model_pattern_matches(pattern: &str, model: &str) -> bool {
    if let Some(prefix) = pattern.strip_suffix('*') {
        model.starts_with(prefix)
    } else {
        pattern == model
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nenjo::Slug;

    #[test]
    fn media_tool_injects_resolved_model() {
        let tool = NativeMediaTool::new(
            ResolvedMediaProvider {
                slug: Slug::derive("image"),
                provider: "openai".to_string(),
                model: "gpt-image-1".to_string(),
                capability: MediaOperation::GenerateImage,
                base_url: None,
            },
            Arc::new(ModelProviderRegistry::new(
                &Default::default(),
                &Default::default(),
            )),
        )
        .expect("tool supported");

        let request = tool
            .request(json!({
                "prompt": "draw a cube"
            }))
            .expect("request parses");

        let NativeMediaRequest::GenerateImage(request) = request else {
            panic!("expected generate image request");
        };
        assert_eq!(request.model, "gpt-image-1");
        assert_eq!(request.prompt, "draw a cube");
    }

    #[test]
    fn media_tool_schema_uses_resolved_provider_tool_spec() {
        let tool = NativeMediaTool::new(
            ResolvedMediaProvider {
                slug: Slug::derive("image"),
                provider: "openai".to_string(),
                model: "gpt-image-1".to_string(),
                capability: MediaOperation::GenerateImage,
                base_url: None,
            },
            Arc::new(ModelProviderRegistry::new(
                &Default::default(),
                &Default::default(),
            )),
        )
        .expect("tool supported");

        let schema = tool.parameters_schema();
        assert!(schema["properties"]["aspect_ratio"].is_null());
        assert_eq!(
            schema["properties"]["provider_options"]["properties"]["quality"]["enum"][0],
            "low"
        );
    }

    #[test]
    fn xai_extend_video_schema_removes_unsupported_dimensions() {
        let tool = NativeMediaTool::new(
            ResolvedMediaProvider {
                slug: Slug::derive("video"),
                provider: "xai".to_string(),
                model: "grok-imagine-video".to_string(),
                capability: MediaOperation::ExtendVideo,
                base_url: None,
            },
            Arc::new(ModelProviderRegistry::new(
                &Default::default(),
                &Default::default(),
            )),
        )
        .expect("tool supported");

        let schema = tool.parameters_schema();
        assert!(schema["properties"]["aspect_ratio"].is_null());
        assert!(schema["properties"]["resolution"].is_null());
        assert_eq!(schema["properties"]["duration_seconds"]["minimum"], 2);
        assert_eq!(schema["properties"]["duration_seconds"]["maximum"], 10);
    }

    #[test]
    fn video_tool_description_explains_async_wait_flow() {
        let tool = NativeMediaTool::new(
            ResolvedMediaProvider {
                slug: Slug::derive("video"),
                provider: "xai".to_string(),
                model: "grok-imagine-video".to_string(),
                capability: MediaOperation::GenerateVideo,
                base_url: None,
            },
            Arc::new(ModelProviderRegistry::new(
                &Default::default(),
                &Default::default(),
            )),
        )
        .expect("tool supported");

        let description = tool.description();
        assert!(description.contains("asynchronous"));
        assert!(description.contains("wait_operations"));
        assert!(description.contains("Do not call generate_video again"));
    }

    #[test]
    fn completed_asset_summary_includes_url() {
        let job = NativeMediaJob {
            provider: "xai".into(),
            operation: MediaOperation::GenerateVideo,
            job_id: "request-123".into(),
            status: NativeMediaJobStatus::Running,
            model: Some("grok-imagine-video".into()),
            metadata: None,
        };
        let summary = completed_assets_summary(
            &job,
            &[MediaOutputAsset::Url {
                url: "https://vidgen.x.ai/example/video.mp4".into(),
                mime_type: Some("video/mp4".into()),
            }],
        );

        assert!(summary.contains("request-123"));
        assert!(summary.contains("https://vidgen.x.ai/example/video.mp4"));
    }
}
