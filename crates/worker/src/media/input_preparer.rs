//! Worker host implementation of ephemeral artifact request preparation.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use nenjo::{ArtifactInputPreparer, PreparedModelArtifacts};
use nenjo_models::{ConversationMessage, PreparedArtifact, PreparedArtifactInputs};
use nenjo_platform::artifacts::{ArtifactMaterializer, PlatformArtifactMaterializer};
use tracing::debug;
use uuid::Uuid;

use super::provider_analyzer::ProviderArtifactAnalyzer;
use super::{
    ArtifactAnalysisRequest, ArtifactAnalyzer, ArtifactInputDisposition, ArtifactInputRoute,
    MaterializedAnalysisInput, ModelAssignmentResolver, PrimaryArtifactModel, ResourceRef,
    collect_artifact_inputs,
};
use crate::bootstrap::{
    load_cached_agent_model_assignments, load_cached_capability_defaults, load_cached_model_runtime,
};
use crate::providers::ModelProviderRegistry;
use crate::tools::platform_payload::PlatformPayloadEncoder;

type WorkerMaterializer = PlatformArtifactMaterializer<PlatformPayloadEncoder>;
type WorkerAnalyzer = ProviderArtifactAnalyzer<Arc<ModelProviderRegistry>>;

/// Worker-scoped preparer using authenticated platform materialization and
/// the canonical assignment/transport routers.
pub struct WorkerArtifactInputPreparer {
    inner: ArtifactInputPreparerCore<WorkerMaterializer, WorkerAnalyzer>,
}

struct ArtifactInputPreparerCore<M, A> {
    org_id: Uuid,
    materializer: Arc<M>,
    manifests_dir: PathBuf,
    providers: Arc<ModelProviderRegistry>,
    analyzer: A,
}

impl WorkerArtifactInputPreparer {
    pub(crate) fn new(
        org_id: Uuid,
        materializer: Arc<WorkerMaterializer>,
        manifests_dir: PathBuf,
        providers: Arc<ModelProviderRegistry>,
    ) -> Self {
        Self {
            inner: ArtifactInputPreparerCore::new(
                org_id,
                materializer,
                manifests_dir,
                providers.clone(),
                ProviderArtifactAnalyzer::new(providers),
            ),
        }
    }
}

impl<M, A> ArtifactInputPreparerCore<M, A>
where
    M: ArtifactMaterializer,
    A: ArtifactAnalyzer,
{
    fn new(
        org_id: Uuid,
        materializer: Arc<M>,
        manifests_dir: PathBuf,
        providers: Arc<ModelProviderRegistry>,
        analyzer: A,
    ) -> Self {
        Self {
            org_id,
            materializer,
            manifests_dir,
            providers,
            analyzer,
        }
    }

    async fn prepare(
        &self,
        messages: &[ConversationMessage],
        agent: &nenjo::manifest::AgentManifest,
        model: &nenjo::manifest::ModelManifest,
    ) -> anyhow::Result<PreparedModelArtifacts> {
        let inputs = collect_artifact_inputs(messages);
        if inputs.is_empty() {
            return Ok(PreparedModelArtifacts::new(
                messages,
                PreparedArtifactInputs::default(),
                Vec::new(),
                Default::default(),
            ));
        }
        // Model and assignment events replace these canonical cache files at
        // runtime. Rebuild the lightweight resolver per request so a long-lived
        // provider never retains its bootstrap snapshot.
        let assignments = ModelAssignmentResolver::new(
            load_cached_model_runtime(&self.manifests_dir),
            load_cached_agent_model_assignments(&self.manifests_dir),
            load_cached_capability_defaults(&self.manifests_dir),
        );
        let route = ArtifactInputRoute::resolve(
            &PrimaryArtifactModel::from(model),
            &inputs,
            self.providers.as_ref(),
            &(
                &assignments,
                ResourceRef::agent(None, Some(agent.slug.as_str())),
            ),
        )?;
        let mut analysis_messages = Vec::with_capacity(route.analysis_batches.len());
        let mut usage = nenjo_models::TokenUsage::default();
        for batch in &route.analysis_batches {
            let mut materialized_inputs = Vec::with_capacity(batch.inputs.len());
            for input in &batch.inputs {
                let materialized = self
                    .materializer
                    .materialize(self.org_id, input.artifact())
                    .await?;
                materialized_inputs.push(MaterializedAnalysisInput::new(
                    input.clone(),
                    materialized.shared_bytes(),
                ));
            }
            let result = self
                .analyzer
                .analyze(ArtifactAnalysisRequest {
                    endpoint: batch.endpoint.clone(),
                    inputs: materialized_inputs,
                })
                .await?;
            debug!(
                analyzer_model = %result.analyzer.model_slug,
                capability = %result.analyzer.capability,
                source_count = result.source_inputs.len(),
                elapsed_ms = u64::try_from(result.elapsed.as_millis()).unwrap_or(u64::MAX),
                input_tokens = result.usage.input_tokens,
                output_tokens = result.usage.output_tokens,
                "Artifact analysis completed"
            );
            usage.input_tokens = usage.input_tokens.saturating_add(result.usage.input_tokens);
            usage.output_tokens = usage
                .output_tokens
                .saturating_add(result.usage.output_tokens);
            analysis_messages.push(result.into_message());
        }

        let direct_count = route
            .ordered
            .iter()
            .filter(|disposition| matches!(disposition, ArtifactInputDisposition::Direct(_)))
            .count();
        let mut prepared = Vec::with_capacity(direct_count);
        for disposition in route.ordered {
            let ArtifactInputDisposition::Direct(direct) = disposition else {
                continue;
            };
            let materialized = self
                .materializer
                .materialize(self.org_id, direct.input.artifact())
                .await?;
            prepared.push(PreparedArtifact::new(
                direct.input.artifact().clone(),
                materialized.shared_bytes(),
            )?);
        }

        Ok(PreparedModelArtifacts::new(
            messages,
            PreparedArtifactInputs::new(prepared),
            analysis_messages,
            usage,
        ))
    }
}

#[async_trait]
impl ArtifactInputPreparer for WorkerArtifactInputPreparer {
    async fn prepare(
        &self,
        messages: &[ConversationMessage],
        agent: &nenjo::manifest::AgentManifest,
        model: &nenjo::manifest::ModelManifest,
    ) -> anyhow::Result<PreparedModelArtifacts> {
        self.inner.prepare(messages, agent, model).await
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use nenjo::Slug;
    use nenjo::manifest::{AgentManifest, ModelManifest, PromptConfig};
    use nenjo_models::{
        ArtifactAnalysisAssignmentSource, ArtifactId, ArtifactInput, ArtifactInputSource,
        ArtifactRef, ArtifactSize, ChatMessage, MediaType, ModelCapabilityId, ModelModality,
        Sha256Digest, TokenUsage, ToolResultMessage,
    };
    use nenjo_platform::artifacts::{ArtifactMaterializationError, MaterializedArtifact};
    use sha2::{Digest, Sha256};

    use super::*;
    use crate::bootstrap::{CachedAgentManifest, CachedModelManifest};
    use crate::config::ReliabilityConfig;

    struct TestAnalyzer {
        calls: Arc<AtomicUsize>,
        usage: TokenUsage,
    }

    #[async_trait]
    impl ArtifactAnalyzer for TestAnalyzer {
        async fn analyze(
            &self,
            request: ArtifactAnalysisRequest,
        ) -> anyhow::Result<super::super::ArtifactAnalysisResult> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(super::super::ArtifactAnalysisResult::from_request(
                &request,
                "A grounded description of the image.",
                self.usage,
                Duration::from_millis(25),
            ))
        }
    }

    struct TestMaterializer {
        reference: ArtifactRef,
        bytes: Arc<[u8]>,
        calls: AtomicUsize,
    }

    #[async_trait]
    impl ArtifactMaterializer for TestMaterializer {
        async fn materialize(
            &self,
            _org_id: Uuid,
            artifact: &ArtifactRef,
        ) -> Result<MaterializedArtifact, ArtifactMaterializationError> {
            assert_eq!(artifact, &self.reference);
            self.calls.fetch_add(1, Ordering::SeqCst);
            MaterializedArtifact::new_verified(artifact.clone(), Arc::clone(&self.bytes))
        }
    }

    fn model() -> ModelManifest {
        ModelManifest {
            name: "vision".into(),
            slug: Slug::derive("vision"),
            description: None,
            model: "gpt-4.1".into(),
            model_provider: "openai".into(),
            temperature: None,
            context_window: None,
            base_url: None,
            native_tools: Vec::new(),
            capabilities: Vec::new(),
            input_modalities: vec![ModelModality::Text, ModelModality::Image],
            output_modalities: vec![ModelModality::Text],
            execution_modes: Vec::new(),
        }
    }

    fn agent() -> AgentManifest {
        AgentManifest {
            name: "reviewer".into(),
            slug: Slug::derive("reviewer"),
            description: None,
            prompt_config: PromptConfig::default(),
            color: None,
            model: Some(Slug::derive("vision")),
            domains: Vec::new(),
            platform_scopes: Vec::new(),
            mcp_servers: Vec::new(),
            script_tools: Vec::new(),
            media: Vec::new(),
            abilities: Vec::new(),
            prompt_locked: false,
            source_type: None,
            metadata: serde_json::Value::Null,
        }
    }

    #[tokio::test]
    async fn direct_image_is_materialized_once_and_prepared_for_the_provider() {
        let bytes: Arc<[u8]> = Arc::from(&b"image bytes"[..]);
        let reference = ArtifactRef::new(
            ArtifactId::parse(Uuid::new_v4()).unwrap(),
            Sha256Digest::parse(&format!("sha256:{:x}", Sha256::digest(&bytes))).unwrap(),
            MediaType::parse("image/png").unwrap(),
            ArtifactSize::new(bytes.len() as u64),
        );
        let materializer = Arc::new(TestMaterializer {
            reference: reference.clone(),
            bytes,
            calls: AtomicUsize::new(0),
        });
        let input = ArtifactInput::new(reference.clone(), ArtifactInputSource::UserAttachment);
        let messages = vec![
            ConversationMessage::chat(ChatMessage::user("inspect").with_artifacts(vec![input])),
            ConversationMessage::tool_result(
                ToolResultMessage::text("call", "same artifact").with_artifact(reference.clone()),
            ),
        ];
        let manifests = tempfile::tempdir().unwrap();
        let registry = Arc::new(ModelProviderRegistry::new(
            &HashMap::new(),
            &ReliabilityConfig::default(),
        ));
        let preparer = ArtifactInputPreparerCore::new(
            Uuid::new_v4(),
            materializer.clone(),
            manifests.path().to_path_buf(),
            registry,
            TestAnalyzer {
                calls: Arc::new(AtomicUsize::new(0)),
                usage: TokenUsage::default(),
            },
        );

        let prepared = preparer
            .prepare(&messages, &agent(), &model())
            .await
            .unwrap();

        assert!(prepared.artifacts.get(&reference).is_some());
        assert_eq!(materializer.calls.load(Ordering::SeqCst), 1);
        assert_eq!(prepared.usage, Default::default());
    }

    #[tokio::test]
    async fn assigned_image_analysis_becomes_durable_context_and_is_not_repeated() {
        let bytes: Arc<[u8]> = Arc::from(&b"image bytes"[..]);
        let reference = ArtifactRef::new(
            ArtifactId::parse(Uuid::new_v4()).unwrap(),
            Sha256Digest::parse(&format!("sha256:{:x}", Sha256::digest(&bytes))).unwrap(),
            MediaType::parse("image/png").unwrap(),
            ArtifactSize::new(bytes.len() as u64),
        );
        let materializer = Arc::new(TestMaterializer {
            reference: reference.clone(),
            bytes,
            calls: AtomicUsize::new(0),
        });
        let analyzer_calls = Arc::new(AtomicUsize::new(0));
        let analyzer_usage = TokenUsage {
            input_tokens: 40,
            output_tokens: 12,
        };
        let messages = vec![ConversationMessage::chat(
            ChatMessage::user("inspect").with_artifacts(vec![ArtifactInput::new(
                reference.clone(),
                ArtifactInputSource::UserAttachment,
            )]),
        )];
        let manifests = tempfile::tempdir().unwrap();
        let model_id = Uuid::new_v4();
        let analyzer_manifest = ModelManifest {
            name: "assigned analyzer".into(),
            slug: Slug::derive("assigned-analyzer"),
            description: None,
            model: "gpt-4.1".into(),
            model_provider: "openai".into(),
            temperature: None,
            context_window: None,
            base_url: None,
            native_tools: Vec::new(),
            capabilities: vec![ModelCapabilityId::AnalyzeImage],
            input_modalities: vec![ModelModality::Text, ModelModality::Image],
            output_modalities: vec![ModelModality::Text],
            execution_modes: Vec::new(),
        };
        std::fs::write(
            manifests.path().join("models.json"),
            serde_json::to_vec(&[CachedModelManifest {
                id: model_id,
                manifest: analyzer_manifest,
            }])
            .unwrap(),
        )
        .unwrap();
        std::fs::write(
            manifests.path().join("agents.json"),
            serde_json::to_vec(&[CachedAgentManifest {
                id: Uuid::new_v4(),
                manifest: agent(),
                model_assignments: vec![nenjo_events::ModelAssignmentBinding {
                    capability: ModelCapabilityId::AnalyzeImage.to_string(),
                    model_id,
                    assignment_source: "local".into(),
                }],
            }])
            .unwrap(),
        )
        .unwrap();
        let registry = Arc::new(ModelProviderRegistry::new(
            &HashMap::new(),
            &ReliabilityConfig::default(),
        ));
        let preparer = ArtifactInputPreparerCore::new(
            Uuid::new_v4(),
            materializer.clone(),
            manifests.path().to_path_buf(),
            registry,
            TestAnalyzer {
                calls: analyzer_calls.clone(),
                usage: analyzer_usage,
            },
        );
        let mut text_model = model();
        text_model.model_provider = "anthropic".into();
        text_model.model = "claude-sonnet-4".into();
        text_model.input_modalities = vec![ModelModality::Text];

        let prepared = preparer
            .prepare(&messages, &agent(), &text_model)
            .await
            .unwrap();

        assert!(prepared.artifacts.is_empty());
        assert_eq!(prepared.usage, analyzer_usage);
        assert_eq!(prepared.new_analysis_messages.len(), 1);
        let analysis = &prepared.new_analysis_messages[0];
        assert_eq!(
            analysis.source_artifacts().collect::<Vec<_>>(),
            [&reference]
        );
        assert_eq!(
            analysis.analyzer.assignment_source,
            ArtifactAnalysisAssignmentSource::Local
        );
        assert_eq!(
            analysis.analyzer.capability,
            ModelCapabilityId::AnalyzeImage
        );
        assert!(
            prepared
                .request_messages
                .iter()
                .all(|message| message.unresolved_artifact_count() == 0)
        );
        assert_eq!(materializer.calls.load(Ordering::SeqCst), 1);
        assert_eq!(analyzer_calls.load(Ordering::SeqCst), 1);

        let mut durable = messages.clone();
        durable.push(ConversationMessage::artifact_analysis(analysis.clone()));
        let replay = preparer
            .prepare(&durable, &agent(), &text_model)
            .await
            .unwrap();

        assert!(replay.new_analysis_messages.is_empty());
        assert_eq!(replay.usage, TokenUsage::default());
        assert!(
            replay
                .request_messages
                .iter()
                .all(|message| message.unresolved_artifact_count() == 0)
        );
        assert_eq!(materializer.calls.load(Ordering::SeqCst), 1);
        assert_eq!(analyzer_calls.load(Ordering::SeqCst), 1);
    }
}
