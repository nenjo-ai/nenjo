use std::sync::Arc;

use anyhow::Context;
use nenjo_models::{ArtifactInputTransport, MediaType, ModelProvider, ProviderMediaCapabilities};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc};
use tracing::debug;

/// Applies one worker-wide physical request budget beneath provider retries.
///
/// The wrapper belongs inside `ReliableProvider`: each retry or fallback must
/// release its permit before backoff so sleeping attempts do not block useful
/// work from other agents.
pub(super) struct AdmissionControlledProvider {
    inner: Box<dyn ModelProvider>,
    permits: Arc<Semaphore>,
    limit: usize,
}

impl AdmissionControlledProvider {
    pub(super) fn new(
        inner: Box<dyn ModelProvider>,
        permits: Arc<Semaphore>,
        limit: usize,
    ) -> Self {
        Self {
            inner,
            permits,
            limit,
        }
    }

    async fn acquire(&self) -> anyhow::Result<OwnedSemaphorePermit> {
        if let Ok(permit) = Arc::clone(&self.permits).try_acquire_owned() {
            return Ok(permit);
        }
        let queued_at = std::time::Instant::now();
        debug!(
            max_concurrent_requests = self.limit,
            "Model request waiting for worker admission capacity"
        );
        let permit = Arc::clone(&self.permits)
            .acquire_owned()
            .await
            .context("model admission controller closed")?;
        debug!(
            max_concurrent_requests = self.limit,
            queued_ms = queued_at.elapsed().as_millis(),
            "Model request admitted after capacity wait"
        );
        Ok(permit)
    }

    async fn acquire_for_stream(
        &self,
        events: &mpsc::Sender<nenjo_models::ProviderStreamEvent>,
    ) -> anyhow::Result<OwnedSemaphorePermit> {
        if let Ok(permit) = Arc::clone(&self.permits).try_acquire_owned() {
            return Ok(permit);
        }
        events
            .send(nenjo_models::ProviderStreamEvent::CapacityWaiting { limit: self.limit })
            .await
            .context("provider stream consumer closed while waiting for capacity")?;
        let permit = self.acquire().await?;
        events
            .send(nenjo_models::ProviderStreamEvent::CapacityAcquired)
            .await
            .context("provider stream consumer closed after capacity was acquired")?;
        Ok(permit)
    }
}

#[async_trait::async_trait]
impl ModelProvider for AdmissionControlledProvider {
    async fn chat(
        &self,
        request: nenjo_models::ChatRequest<'_>,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<nenjo_models::ChatResponse> {
        let _permit = self.acquire().await?;
        self.inner.chat(request, model, temperature).await
    }

    async fn chat_stream(
        &self,
        request: nenjo_models::ChatRequest<'_>,
        model: &str,
        temperature: f64,
        events: mpsc::Sender<nenjo_models::ProviderStreamEvent>,
    ) -> anyhow::Result<nenjo_models::ChatResponse> {
        let _permit = self.acquire_for_stream(&events).await?;
        self.inner
            .chat_stream(request, model, temperature, events)
            .await
    }

    fn context_window(&self, model: &str) -> Option<usize> {
        self.inner.context_window(model)
    }

    fn supports_native_tools(&self) -> bool {
        self.inner.supports_native_tools()
    }

    fn supports_developer_role(&self, model: &str) -> bool {
        self.inner.supports_developer_role(model)
    }

    fn artifact_input_transport(
        &self,
        model: &str,
        capability: nenjo_models::ModelCapabilityId,
        media_type: &MediaType,
    ) -> ArtifactInputTransport {
        self.inner
            .artifact_input_transport(model, capability, media_type)
    }

    fn media_capabilities(&self) -> Option<ProviderMediaCapabilities> {
        self.inner.media_capabilities()
    }

    async fn submit_media(
        &self,
        request: nenjo_models::NativeMediaRequest,
    ) -> anyhow::Result<nenjo_models::NativeMediaResponse> {
        let _permit = self.acquire().await?;
        self.inner.submit_media(request).await
    }

    async fn poll_media_job(
        &self,
        job: &nenjo_models::NativeMediaJob,
    ) -> anyhow::Result<nenjo_models::NativeMediaResponse> {
        let _permit = self.acquire().await?;
        self.inner.poll_media_job(job).await
    }

    async fn warmup(&self) -> anyhow::Result<()> {
        self.inner.warmup().await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use nenjo_models::{ChatRequest, ChatResponse, FinishReason, TokenUsage};

    use super::*;

    struct ConcurrencyProbe {
        active: Arc<AtomicUsize>,
        max_observed: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl ModelProvider for ConcurrencyProbe {
        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<ChatResponse> {
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_observed.fetch_max(active, Ordering::SeqCst);
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            self.active.fetch_sub(1, Ordering::SeqCst);
            Ok(ChatResponse {
                text: Some("ok".into()),
                tool_calls: Vec::new(),
                provider_tool_calls: Vec::new(),
                usage: TokenUsage::default(),
                finish_reason: FinishReason::Stop,
            })
        }
    }

    #[tokio::test]
    async fn caps_parallel_physical_provider_calls() {
        let active = Arc::new(AtomicUsize::new(0));
        let max_observed = Arc::new(AtomicUsize::new(0));
        let provider = Arc::new(AdmissionControlledProvider::new(
            Box::new(ConcurrencyProbe {
                active: Arc::clone(&active),
                max_observed: Arc::clone(&max_observed),
            }),
            Arc::new(Semaphore::new(1)),
            1,
        ));
        let messages = Vec::new();
        let request = ChatRequest {
            messages: &messages,
            tools: None,
            native_tools: None,
            prepared_artifacts: None,
        };

        let (first, second) = tokio::join!(
            provider.chat(request, "test", 0.0),
            provider.chat(request, "test", 0.0),
        );

        first.unwrap();
        second.unwrap();
        assert_eq!(max_observed.load(Ordering::SeqCst), 1);
    }
}
