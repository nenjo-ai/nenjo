use std::collections::HashMap;
use std::str::FromStr;

use nenjo::Slug;
use nenjo::manifest::{AgentManifest, DomainManifest, MediaRequirement};
use nenjo_events::{ModelAssignmentBinding, ModelCapabilityDefaultBinding};
use nenjo_models::{MediaOperation, ProviderMediaCapabilities};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::config::MediaProviderConfig;

pub trait MediaCapabilitySource {
    fn media_capabilities(&self, provider_name: &str) -> Option<ProviderMediaCapabilities>;
}

/// Concrete provider/model binding for a media capability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedMediaProvider {
    pub slug: Slug,
    pub provider: String,
    pub model: String,
    pub capability: MediaOperation,
    /// Optional provider base URL override (openai-compatible / custom endpoints).
    pub base_url: Option<String>,
}

// ---------------------------------------------------------------------------
// Model assignment resolution (unified path)
// ---------------------------------------------------------------------------

/// Source of a resolved model endpoint for a non-chat capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssignmentSource {
    Local,
    Package,
    OrgDefault,
}

impl AssignmentSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Package => "package",
            Self::OrgDefault => "org_default",
        }
    }
}

impl FromStr for AssignmentSource {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim() {
            "local" => Ok(Self::Local),
            "package" => Ok(Self::Package),
            "org_default" => Ok(Self::OrgDefault),
            other => Err(format!("unknown assignment source '{other}'")),
        }
    }
}

/// Runtime model inventory entry keyed by platform `model_id`.
///
/// Carries capability metadata and `base_url` from bootstrap models so media
/// routing does not depend solely on `ModelManifest`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelRuntimeConfig {
    pub id: Uuid,
    pub slug: Slug,
    pub model: String,
    pub model_provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
}

/// Agent-owned configured model assignments loaded from the canonical agent cache.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentModelAssignments {
    pub agent_id: Uuid,
    pub agent_slug: Slug,
    pub assignments: Vec<ModelAssignmentBinding>,
}

/// Resolved endpoint for a media / non-chat operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedModelEndpoint {
    pub model_id: Uuid,
    pub provider: String,
    pub model: String,
    pub base_url: Option<String>,
    pub capability: MediaOperation,
    pub source: AssignmentSource,
    /// Display slug when known (model slug).
    pub slug: Slug,
}

impl ResolvedModelEndpoint {
    /// Convert into the legacy tool binding shape used by media tools.
    pub fn to_media_provider(&self) -> ResolvedMediaProvider {
        ResolvedMediaProvider {
            slug: self.slug.clone(),
            provider: self.provider.clone(),
            model: self.model.clone(),
            capability: self.capability,
            base_url: self.base_url.clone(),
        }
    }
}

/// Agent identity used when looking up model assignments.
///
/// Assignment matching is agent-only. `resource_type` must be `"agent"`.
#[derive(Debug, Clone, Copy)]
pub struct ResourceRef<'a> {
    pub resource_type: &'a str,
    pub resource_id: Option<Uuid>,
    pub resource_slug: Option<&'a str>,
}

impl<'a> ResourceRef<'a> {
    pub fn agent(agent_id: Option<Uuid>, agent_slug: Option<&'a str>) -> Self {
        Self {
            resource_type: "agent",
            resource_id: agent_id,
            resource_slug: agent_slug,
        }
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ModelAssignmentResolveError {
    #[error("no model assignment for capability {capability:?}")]
    MissingAssignment { capability: MediaOperation },
    #[error("assigned model {model_id} lacks capability {capability:?}")]
    ModelLacksCapability {
        capability: MediaOperation,
        model_id: Uuid,
    },
    #[error("assigned model {model_id} is missing from the bootstrap model inventory")]
    ModelNotFound { model_id: Uuid },
}

/// Resolves non-chat capabilities from agent-owned model assignments and org defaults.
///
/// Order: **first local → first package → org default → error**.
///
/// Never scans the full model inventory for an unassigned capability.
#[derive(Clone)]
pub struct ModelAssignmentResolver {
    models_by_id: HashMap<Uuid, ModelRuntimeConfig>,
    assignments: Vec<AgentModelAssignments>,
    defaults: HashMap<String, Uuid>,
}

impl ModelAssignmentResolver {
    pub fn new(
        models: impl IntoIterator<Item = ModelRuntimeConfig>,
        assignments: Vec<AgentModelAssignments>,
        defaults: impl IntoIterator<Item = ModelCapabilityDefaultBinding>,
    ) -> Self {
        let models_by_id = models.into_iter().map(|m| (m.id, m)).collect();
        let defaults = defaults
            .into_iter()
            .map(|row| (row.capability, row.model_id))
            .collect();
        Self {
            models_by_id,
            assignments,
            defaults,
        }
    }

    /// Capabilities explicitly assigned to a resource (local + package).
    pub fn assigned_capabilities(&self, resource: ResourceRef<'_>) -> Vec<MediaOperation> {
        let mut caps = Vec::new();
        let Some(agent) = self.assignment_set(resource) else {
            return caps;
        };
        for assignment in &agent.assignments {
            if let Ok(cap) = MediaOperation::from_str(&assignment.capability)
                && !caps.contains(&cap)
            {
                caps.push(cap);
            }
        }
        caps
    }

    /// Resolve a capability for a resource without inventory-scanning models.
    pub fn resolve(
        &self,
        resource: ResourceRef<'_>,
        capability: MediaOperation,
    ) -> Result<ResolvedModelEndpoint, ModelAssignmentResolveError> {
        let cap_str = capability.as_str();

        if let Some(assignment) = self.find_assignment(resource, cap_str, "local") {
            return self.endpoint_for(assignment.model_id, capability, AssignmentSource::Local);
        }
        if let Some(assignment) = self.find_assignment(resource, cap_str, "package") {
            return self.endpoint_for(assignment.model_id, capability, AssignmentSource::Package);
        }

        if let Some(model_id) = self.defaults.get(cap_str) {
            return self.endpoint_for(*model_id, capability, AssignmentSource::OrgDefault);
        }

        Err(ModelAssignmentResolveError::MissingAssignment { capability })
    }

    fn find_assignment(
        &self,
        resource: ResourceRef<'_>,
        capability: &str,
        source: &str,
    ) -> Option<&ModelAssignmentBinding> {
        self.assignment_set(resource)?
            .assignments
            .iter()
            .find(|row| row.capability == capability && row.assignment_source == source)
    }

    fn assignment_set(&self, resource: ResourceRef<'_>) -> Option<&AgentModelAssignments> {
        if resource.resource_type != "agent" {
            return None;
        }
        self.assignments.iter().find(|assignments| {
            resource
                .resource_id
                .is_some_and(|id| assignments.agent_id == id)
                || resource
                    .resource_slug
                    .is_some_and(|slug| assignments.agent_slug.as_str() == slug)
        })
    }

    fn endpoint_for(
        &self,
        model_id: Uuid,
        capability: MediaOperation,
        source: AssignmentSource,
    ) -> Result<ResolvedModelEndpoint, ModelAssignmentResolveError> {
        let Some(model) = self.models_by_id.get(&model_id) else {
            return Err(ModelAssignmentResolveError::ModelNotFound { model_id });
        };
        let has_cap = model
            .capabilities
            .iter()
            .any(|c| c.trim() == capability.as_str());
        if !has_cap {
            return Err(ModelAssignmentResolveError::ModelLacksCapability {
                capability,
                model_id,
            });
        }
        Ok(ResolvedModelEndpoint {
            model_id,
            provider: model.model_provider.clone(),
            model: model.model.clone(),
            base_url: model.base_url.clone(),
            capability,
            source,
            slug: model.slug.clone(),
        })
    }
}

/// Resolves agent media requirements against worker media provider config.
#[derive(Clone)]
pub struct MediaProviderResolver<'a> {
    providers: Vec<MediaProviderConfig>,
    capability_source: &'a dyn MediaCapabilitySource,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum MediaResolutionError {
    #[error(
        "no configured media provider supports {capability:?} with provider {provider:?} and model {model:?}"
    )]
    MissingCapability {
        capability: MediaOperation,
        provider: Option<String>,
        model: Option<String>,
    },
    #[error(
        "multiple configured media providers support {capability:?}; pin provider or model on the media requirement"
    )]
    AmbiguousCapability { capability: MediaOperation },
}

impl<'a> MediaProviderResolver<'a> {
    pub fn new(
        providers: Vec<MediaProviderConfig>,
        capability_source: &'a dyn MediaCapabilitySource,
    ) -> Self {
        Self {
            providers,
            capability_source,
        }
    }

    pub fn resolve(
        &self,
        requirement: &MediaRequirement,
    ) -> Result<ResolvedMediaProvider, MediaResolutionError> {
        let capability = requirement.capability();
        let provider = requirement.provider();
        let model = requirement.model();

        let mut matches = self.providers.iter().filter(|candidate| {
            configured_for_capability(candidate, capability)
                && provider_supports_capability(self.capability_source, candidate, capability)
                && provider.is_none_or(|required| candidate.provider == required)
                && model.is_none_or(|required| candidate.model == required)
        });
        let Some(first) = matches.next() else {
            return Err(MediaResolutionError::MissingCapability {
                capability,
                provider: provider.map(str::to_string),
                model: model.map(str::to_string),
            });
        };
        if matches.next().is_some() {
            return Err(MediaResolutionError::AmbiguousCapability { capability });
        }
        Ok(resolved_provider(first, capability))
    }

    pub fn validate_agent_media(
        &self,
        agent: &AgentManifest,
    ) -> Result<Vec<ResolvedMediaProvider>, MediaResolutionError> {
        agent
            .media
            .iter()
            .map(|requirement| self.resolve(requirement))
            .collect()
    }

    pub fn validate_domain_media(
        &self,
        domain: &DomainManifest,
    ) -> Result<Vec<ResolvedMediaProvider>, MediaResolutionError> {
        domain
            .media
            .iter()
            .map(|requirement| self.resolve(requirement))
            .collect()
    }
}

pub fn validate_agent_media(
    resolver: &MediaProviderResolver<'_>,
    agent: &AgentManifest,
) -> Result<Vec<ResolvedMediaProvider>, MediaResolutionError> {
    resolver.validate_agent_media(agent)
}

pub fn validate_domain_media(
    resolver: &MediaProviderResolver<'_>,
    domain: &DomainManifest,
) -> Result<Vec<ResolvedMediaProvider>, MediaResolutionError> {
    resolver.validate_domain_media(domain)
}

fn configured_for_capability(provider: &MediaProviderConfig, capability: MediaOperation) -> bool {
    provider.capabilities.contains(&capability)
}

fn provider_supports_capability(
    capability_source: &dyn MediaCapabilitySource,
    provider: &MediaProviderConfig,
    capability: MediaOperation,
) -> bool {
    capability_source
        .media_capabilities(&provider.provider)
        .is_some_and(|capabilities| {
            capabilities.models.into_iter().any(|model| {
                model_pattern_matches(&model.model_pattern, &provider.model)
                    && model.operations().any(|operation| operation == capability)
            })
        })
}

fn resolved_provider(
    provider: &MediaProviderConfig,
    capability: MediaOperation,
) -> ResolvedMediaProvider {
    ResolvedMediaProvider {
        slug: provider.slug.clone(),
        provider: provider.provider.clone(),
        model: provider.model.clone(),
        capability,
        base_url: None,
    }
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
    use nenjo::manifest::{
        DomainManifest, DomainPromptConfig, MediaBindingRequirement, MediaRequirement,
    };
    use nenjo_models::{
        MediaExecutionMode, MediaToolSpec, ModelMediaCapabilities, ProviderMediaCapabilities,
    };

    struct StaticCapabilitySource {
        providers: Vec<ProviderMediaCapabilities>,
    }

    impl StaticCapabilitySource {
        fn standard() -> Self {
            Self {
                providers: vec![
                    provider_capabilities(
                        "openai",
                        vec![model_capabilities(
                            "gpt-image-*",
                            vec![MediaOperation::GenerateImage],
                        )],
                    ),
                    provider_capabilities(
                        "xai",
                        vec![
                            model_capabilities(
                                "grok-imagine-image-*",
                                vec![MediaOperation::GenerateImage],
                            ),
                            model_capabilities(
                                "grok-imagine-video*",
                                vec![MediaOperation::ReferenceToVideo],
                            ),
                        ],
                    ),
                ],
            }
        }
    }

    impl MediaCapabilitySource for StaticCapabilitySource {
        fn media_capabilities(&self, provider_name: &str) -> Option<ProviderMediaCapabilities> {
            self.providers
                .iter()
                .find(|provider| provider.provider == provider_name)
                .cloned()
        }
    }

    fn provider_capabilities(
        provider: &str,
        models: Vec<ModelMediaCapabilities>,
    ) -> ProviderMediaCapabilities {
        ProviderMediaCapabilities {
            provider: provider.to_string(),
            model_tools: Vec::new(),
            models,
        }
    }

    fn model_capabilities(
        model_pattern: &str,
        operations: Vec<MediaOperation>,
    ) -> ModelMediaCapabilities {
        ModelMediaCapabilities {
            model_pattern: model_pattern.to_string(),
            tools: operations
                .into_iter()
                .map(|operation| MediaToolSpec {
                    capability: operation,
                    tool_name: operation.as_str().to_string(),
                    description: format!("test {operation:?}"),
                    parameters_schema: serde_json::json!({
                        "type": "object",
                        "properties": {}
                    }),
                    execution: MediaExecutionMode::Immediate,
                })
                .collect(),
        }
    }

    fn provider(
        slug: &str,
        provider: &str,
        model: &str,
        capabilities: Vec<MediaOperation>,
    ) -> MediaProviderConfig {
        MediaProviderConfig {
            slug: Slug::derive(slug),
            provider: provider.to_string(),
            model: model.to_string(),
            capabilities,
        }
    }

    #[test]
    fn rejects_ambiguous_unpinned_capability() {
        let capabilities = StaticCapabilitySource::standard();
        let resolver = MediaProviderResolver::new(
            vec![
                provider(
                    "xai_image",
                    "xai",
                    "grok-imagine-image-quality",
                    vec![MediaOperation::GenerateImage],
                ),
                provider(
                    "openai_image",
                    "openai",
                    "gpt-image-1",
                    vec![MediaOperation::GenerateImage],
                ),
            ],
            &capabilities,
        );

        let err = resolver
            .resolve(&MediaRequirement::Capability(MediaOperation::GenerateImage))
            .unwrap_err();

        assert_eq!(
            err,
            MediaResolutionError::AmbiguousCapability {
                capability: MediaOperation::GenerateImage,
            }
        );
    }

    #[test]
    fn resolves_unpinned_capability_when_only_one_provider_matches() {
        let capabilities = StaticCapabilitySource::standard();
        let resolver = MediaProviderResolver::new(
            vec![provider(
                "xai_video",
                "xai",
                "grok-imagine-video",
                vec![MediaOperation::ReferenceToVideo],
            )],
            &capabilities,
        );

        let resolved = resolver
            .resolve(&MediaRequirement::Capability(
                MediaOperation::ReferenceToVideo,
            ))
            .unwrap();

        assert_eq!(resolved.slug, Slug::derive("xai_video"));
    }

    #[test]
    fn provider_constraint_selects_matching_provider() {
        let capabilities = StaticCapabilitySource::standard();
        let resolver = MediaProviderResolver::new(
            vec![
                provider(
                    "openai_image",
                    "openai",
                    "gpt-image-1",
                    vec![MediaOperation::GenerateImage],
                ),
                provider(
                    "xai_image",
                    "xai",
                    "grok-imagine-image-quality",
                    vec![MediaOperation::GenerateImage],
                ),
            ],
            &capabilities,
        );

        let resolved = resolver
            .resolve(&MediaRequirement::Binding(MediaBindingRequirement {
                capability: MediaOperation::GenerateImage,
                provider: Some("xai".to_string()),
                model: None,
            }))
            .unwrap();

        assert_eq!(resolved.slug, Slug::derive("xai_image"));
    }

    #[test]
    fn provider_constraint_rejects_multiple_matching_models() {
        let capabilities = StaticCapabilitySource::standard();
        let resolver = MediaProviderResolver::new(
            vec![
                provider(
                    "xai_image_quality",
                    "xai",
                    "grok-imagine-image-quality",
                    vec![MediaOperation::GenerateImage],
                ),
                provider(
                    "xai_image_fast",
                    "xai",
                    "grok-imagine-image-fast",
                    vec![MediaOperation::GenerateImage],
                ),
            ],
            &capabilities,
        );

        let err = resolver
            .resolve(&MediaRequirement::Binding(MediaBindingRequirement {
                capability: MediaOperation::GenerateImage,
                provider: Some("xai".to_string()),
                model: None,
            }))
            .unwrap_err();

        assert_eq!(
            err,
            MediaResolutionError::AmbiguousCapability {
                capability: MediaOperation::GenerateImage,
            }
        );
    }

    #[test]
    fn model_constraint_must_match_configured_provider() {
        let capabilities = StaticCapabilitySource::standard();
        let resolver = MediaProviderResolver::new(
            vec![provider(
                "xai_video",
                "xai",
                "grok-imagine-video",
                vec![MediaOperation::ReferenceToVideo],
            )],
            &capabilities,
        );

        let err = resolver
            .resolve(&MediaRequirement::Binding(MediaBindingRequirement {
                capability: MediaOperation::ReferenceToVideo,
                provider: Some("xai".to_string()),
                model: Some("other-model".to_string()),
            }))
            .unwrap_err();

        assert_eq!(
            err,
            MediaResolutionError::MissingCapability {
                capability: MediaOperation::ReferenceToVideo,
                provider: Some("xai".to_string()),
                model: Some("other-model".to_string()),
            }
        );
    }

    #[test]
    fn validates_domain_media_requirements() {
        let capabilities = StaticCapabilitySource::standard();
        let resolver = MediaProviderResolver::new(
            vec![provider(
                "xai_video",
                "xai",
                "grok-imagine-video",
                vec![MediaOperation::ReferenceToVideo],
            )],
            &capabilities,
        );
        let domain = DomainManifest {
            slug: Slug::derive("creative"),
            name: "creative".to_string(),
            path: "domains".to_string(),
            description: None,
            command: "#creative".to_string(),
            platform_scopes: Vec::new(),
            abilities: Vec::new(),
            mcp_servers: Vec::new(),
            script_tools: Vec::new(),
            media: vec![MediaRequirement::Capability(
                MediaOperation::ReferenceToVideo,
            )],
            prompt_config: DomainPromptConfig::default(),
        };

        let resolved = resolver.validate_domain_media(&domain).unwrap();

        assert_eq!(resolved[0].slug, Slug::derive("xai_video"));
    }

    #[test]
    fn provider_capability_truth_overrides_configured_claims() {
        let capabilities = StaticCapabilitySource::standard();
        let resolver = MediaProviderResolver::new(
            vec![provider(
                "xai_image_claims_video",
                "xai",
                "grok-imagine-image-quality",
                vec![MediaOperation::ReferenceToVideo],
            )],
            &capabilities,
        );

        let err = resolver
            .resolve(&MediaRequirement::Capability(
                MediaOperation::ReferenceToVideo,
            ))
            .unwrap_err();

        assert_eq!(
            err,
            MediaResolutionError::MissingCapability {
                capability: MediaOperation::ReferenceToVideo,
                provider: None,
                model: None,
            }
        );
    }

    fn stt_model(id: Uuid, base_url: Option<&str>) -> ModelRuntimeConfig {
        ModelRuntimeConfig {
            id,
            slug: Slug::derive("custom-stt"),
            model: "whisper-1".to_string(),
            model_provider: "openai".to_string(),
            base_url: base_url.map(str::to_string),
            capabilities: vec![MediaOperation::TranscribeAudio.as_str().to_string()],
        }
    }

    fn agent_assignments(
        agent_id: Uuid,
        slug: &str,
        assignments: Vec<ModelAssignmentBinding>,
    ) -> AgentModelAssignments {
        AgentModelAssignments {
            agent_id,
            agent_slug: Slug::derive(slug),
            assignments,
        }
    }

    fn assignment(
        capability: MediaOperation,
        model_id: Uuid,
        source: &str,
    ) -> ModelAssignmentBinding {
        ModelAssignmentBinding {
            capability: capability.as_str().to_string(),
            model_id,
            assignment_source: source.to_string(),
        }
    }

    #[test]
    fn assignment_only_custom_base_url_stt_resolves_without_media_providers() {
        let model_id = Uuid::new_v4();
        let agent_id = Uuid::new_v4();
        let resolver = ModelAssignmentResolver::new(
            vec![stt_model(model_id, Some("https://stt.example.internal/v1"))],
            vec![agent_assignments(
                agent_id,
                "voice-agent",
                vec![assignment(
                    MediaOperation::TranscribeAudio,
                    model_id,
                    "local",
                )],
            )],
            Vec::new(),
        );

        let resolved = resolver
            .resolve(
                ResourceRef {
                    resource_type: "agent",
                    resource_id: Some(agent_id),
                    resource_slug: Some("voice-agent"),
                },
                MediaOperation::TranscribeAudio,
            )
            .expect("assignment-only STT path should resolve");

        assert_eq!(resolved.model_id, model_id);
        assert_eq!(resolved.provider, "openai");
        assert_eq!(resolved.model, "whisper-1");
        assert_eq!(
            resolved.base_url.as_deref(),
            Some("https://stt.example.internal/v1")
        );
        assert_eq!(resolved.source, AssignmentSource::Local);
        assert_eq!(resolved.capability, MediaOperation::TranscribeAudio);
    }

    #[test]
    fn prefers_local_assignment_over_package_and_default() {
        let local_id = Uuid::new_v4();
        let package_id = Uuid::new_v4();
        let default_id = Uuid::new_v4();
        let agent_id = Uuid::new_v4();
        let models = vec![
            ModelRuntimeConfig {
                id: local_id,
                slug: Slug::derive("local-img"),
                model: "gpt-image-1".to_string(),
                model_provider: "openai".to_string(),
                base_url: None,
                capabilities: vec![MediaOperation::GenerateImage.as_str().to_string()],
            },
            ModelRuntimeConfig {
                id: package_id,
                slug: Slug::derive("pkg-img"),
                model: "grok-imagine-image".to_string(),
                model_provider: "xai".to_string(),
                base_url: None,
                capabilities: vec![MediaOperation::GenerateImage.as_str().to_string()],
            },
            ModelRuntimeConfig {
                id: default_id,
                slug: Slug::derive("default-img"),
                model: "gpt-image-1".to_string(),
                model_provider: "openai".to_string(),
                base_url: None,
                capabilities: vec![MediaOperation::GenerateImage.as_str().to_string()],
            },
        ];
        let resolver = ModelAssignmentResolver::new(
            models,
            vec![agent_assignments(
                agent_id,
                "image-agent",
                vec![
                    assignment(MediaOperation::GenerateImage, package_id, "package"),
                    assignment(MediaOperation::GenerateImage, local_id, "local"),
                ],
            )],
            vec![ModelCapabilityDefaultBinding {
                capability: MediaOperation::GenerateImage.as_str().to_string(),
                model_id: default_id,
            }],
        );

        let resolved = resolver
            .resolve(
                ResourceRef {
                    resource_type: "agent",
                    resource_id: Some(agent_id),
                    resource_slug: None,
                },
                MediaOperation::GenerateImage,
            )
            .unwrap();

        assert_eq!(resolved.model_id, local_id);
        assert_eq!(resolved.source, AssignmentSource::Local);
    }

    #[test]
    fn falls_back_to_org_default_when_no_resource_assignment() {
        let default_id = Uuid::new_v4();
        let agent_id = Uuid::new_v4();
        let resolver = ModelAssignmentResolver::new(
            vec![stt_model(default_id, None)],
            Vec::new(),
            vec![ModelCapabilityDefaultBinding {
                capability: MediaOperation::TranscribeAudio.as_str().to_string(),
                model_id: default_id,
            }],
        );

        let resolved = resolver
            .resolve(
                ResourceRef {
                    resource_type: "agent",
                    resource_id: Some(agent_id),
                    resource_slug: None,
                },
                MediaOperation::TranscribeAudio,
            )
            .unwrap();

        assert_eq!(resolved.model_id, default_id);
        assert_eq!(resolved.source, AssignmentSource::OrgDefault);
    }

    #[test]
    fn resource_scoped_resolve_never_inventory_scans_models() {
        let model_id = Uuid::new_v4();
        let agent_id = Uuid::new_v4();
        // Model advertises STT but is not assigned and not a default.
        let resolver = ModelAssignmentResolver::new(
            vec![stt_model(model_id, Some("https://stt.example/v1"))],
            Vec::new(),
            Vec::new(),
        );

        let err = resolver
            .resolve(
                ResourceRef {
                    resource_type: "agent",
                    resource_id: Some(agent_id),
                    resource_slug: None,
                },
                MediaOperation::TranscribeAudio,
            )
            .unwrap_err();

        assert_eq!(
            err,
            ModelAssignmentResolveError::MissingAssignment {
                capability: MediaOperation::TranscribeAudio,
            }
        );
    }

    #[test]
    fn missing_assignment_errors_without_default() {
        let resolver = ModelAssignmentResolver::new(Vec::new(), Vec::new(), Vec::new());
        let err = resolver
            .resolve(
                ResourceRef {
                    resource_type: "agent",
                    resource_id: Some(Uuid::new_v4()),
                    resource_slug: None,
                },
                MediaOperation::GenerateImage,
            )
            .unwrap_err();
        assert!(matches!(
            err,
            ModelAssignmentResolveError::MissingAssignment { .. }
        ));
    }
}
