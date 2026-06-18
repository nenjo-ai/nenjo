use nenjo::Slug;
use nenjo::manifest::{AgentManifest, DomainManifest, MediaRequirement};
use nenjo_models::{NativeOperation, ProviderNativeCapabilities};
use thiserror::Error;

use crate::config::MediaProviderConfig;

pub trait MediaCapabilitySource {
    fn native_capabilities(&self, provider_name: &str) -> Option<ProviderNativeCapabilities>;
}

/// Concrete provider/model binding for a native media capability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedMediaProvider {
    pub slug: Slug,
    pub provider: String,
    pub model: String,
    pub capability: NativeOperation,
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
        capability: NativeOperation,
        provider: Option<String>,
        model: Option<String>,
    },
    #[error(
        "multiple configured media providers support {capability:?}; pin provider or model on the media requirement"
    )]
    AmbiguousCapability { capability: NativeOperation },
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

fn configured_for_capability(provider: &MediaProviderConfig, capability: NativeOperation) -> bool {
    provider.capabilities.contains(&capability)
}

fn provider_supports_capability(
    capability_source: &dyn MediaCapabilitySource,
    provider: &MediaProviderConfig,
    capability: NativeOperation,
) -> bool {
    capability_source
        .native_capabilities(&provider.provider)
        .is_some_and(|capabilities| {
            capabilities.models.into_iter().any(|model| {
                model_pattern_matches(&model.model_pattern, &provider.model)
                    && model.operations().any(|operation| operation == capability)
            })
        })
}

fn resolved_provider(
    provider: &MediaProviderConfig,
    capability: NativeOperation,
) -> ResolvedMediaProvider {
    ResolvedMediaProvider {
        slug: provider.slug.clone(),
        provider: provider.provider.clone(),
        model: provider.model.clone(),
        capability,
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
        ModelNativeCapabilities, NativeExecutionMode, NativeToolSpec, ProviderNativeCapabilities,
    };

    struct StaticCapabilitySource {
        providers: Vec<ProviderNativeCapabilities>,
    }

    impl StaticCapabilitySource {
        fn standard() -> Self {
            Self {
                providers: vec![
                    provider_capabilities(
                        "openai",
                        vec![model_capabilities(
                            "gpt-image-*",
                            vec![NativeOperation::GenerateImage],
                        )],
                    ),
                    provider_capabilities(
                        "xai",
                        vec![
                            model_capabilities(
                                "grok-imagine-image-*",
                                vec![NativeOperation::GenerateImage],
                            ),
                            model_capabilities(
                                "grok-imagine-video*",
                                vec![NativeOperation::ReferenceToVideo],
                            ),
                        ],
                    ),
                ],
            }
        }
    }

    impl MediaCapabilitySource for StaticCapabilitySource {
        fn native_capabilities(&self, provider_name: &str) -> Option<ProviderNativeCapabilities> {
            self.providers
                .iter()
                .find(|provider| provider.provider == provider_name)
                .cloned()
        }
    }

    fn provider_capabilities(
        provider: &str,
        models: Vec<ModelNativeCapabilities>,
    ) -> ProviderNativeCapabilities {
        ProviderNativeCapabilities {
            provider: provider.to_string(),
            model_tools: Vec::new(),
            models,
        }
    }

    fn model_capabilities(
        model_pattern: &str,
        operations: Vec<NativeOperation>,
    ) -> ModelNativeCapabilities {
        ModelNativeCapabilities {
            model_pattern: model_pattern.to_string(),
            tools: operations
                .into_iter()
                .map(|operation| NativeToolSpec {
                    capability: operation,
                    tool_name: operation.as_str().to_string(),
                    description: format!("test {operation:?}"),
                    parameters_schema: serde_json::json!({
                        "type": "object",
                        "properties": {}
                    }),
                    execution: NativeExecutionMode::Immediate,
                })
                .collect(),
        }
    }

    fn provider(
        slug: &str,
        provider: &str,
        model: &str,
        capabilities: Vec<NativeOperation>,
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
                    vec![NativeOperation::GenerateImage],
                ),
                provider(
                    "openai_image",
                    "openai",
                    "gpt-image-1",
                    vec![NativeOperation::GenerateImage],
                ),
            ],
            &capabilities,
        );

        let err = resolver
            .resolve(&MediaRequirement::Capability(
                NativeOperation::GenerateImage,
            ))
            .unwrap_err();

        assert_eq!(
            err,
            MediaResolutionError::AmbiguousCapability {
                capability: NativeOperation::GenerateImage,
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
                vec![NativeOperation::ReferenceToVideo],
            )],
            &capabilities,
        );

        let resolved = resolver
            .resolve(&MediaRequirement::Capability(
                NativeOperation::ReferenceToVideo,
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
                    vec![NativeOperation::GenerateImage],
                ),
                provider(
                    "xai_image",
                    "xai",
                    "grok-imagine-image-quality",
                    vec![NativeOperation::GenerateImage],
                ),
            ],
            &capabilities,
        );

        let resolved = resolver
            .resolve(&MediaRequirement::Binding(MediaBindingRequirement {
                capability: NativeOperation::GenerateImage,
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
                    vec![NativeOperation::GenerateImage],
                ),
                provider(
                    "xai_image_fast",
                    "xai",
                    "grok-imagine-image-fast",
                    vec![NativeOperation::GenerateImage],
                ),
            ],
            &capabilities,
        );

        let err = resolver
            .resolve(&MediaRequirement::Binding(MediaBindingRequirement {
                capability: NativeOperation::GenerateImage,
                provider: Some("xai".to_string()),
                model: None,
            }))
            .unwrap_err();

        assert_eq!(
            err,
            MediaResolutionError::AmbiguousCapability {
                capability: NativeOperation::GenerateImage,
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
                vec![NativeOperation::ReferenceToVideo],
            )],
            &capabilities,
        );

        let err = resolver
            .resolve(&MediaRequirement::Binding(MediaBindingRequirement {
                capability: NativeOperation::ReferenceToVideo,
                provider: Some("xai".to_string()),
                model: Some("other-model".to_string()),
            }))
            .unwrap_err();

        assert_eq!(
            err,
            MediaResolutionError::MissingCapability {
                capability: NativeOperation::ReferenceToVideo,
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
                vec![NativeOperation::ReferenceToVideo],
            )],
            &capabilities,
        );
        let domain = DomainManifest {
            name: "creative".to_string(),
            path: "domains".to_string(),
            description: None,
            command: "#creative".to_string(),
            platform_scopes: Vec::new(),
            abilities: Vec::new(),
            mcp_servers: Vec::new(),
            script_tools: Vec::new(),
            media: vec![MediaRequirement::Capability(
                NativeOperation::ReferenceToVideo,
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
                vec![NativeOperation::ReferenceToVideo],
            )],
            &capabilities,
        );

        let err = resolver
            .resolve(&MediaRequirement::Capability(
                NativeOperation::ReferenceToVideo,
            ))
            .unwrap_err();

        assert_eq!(
            err,
            MediaResolutionError::MissingCapability {
                capability: NativeOperation::ReferenceToVideo,
                provider: None,
                model: None,
            }
        );
    }
}
