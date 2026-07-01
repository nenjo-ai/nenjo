use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, RwLock};

use crate::config::Config;
use crate::external_mcp::ExternalMcpPool;
use crate::media::MediaProviderResolver;
use crate::providers::ModelProviderRegistry;
use crate::skills::{LocalSkillProvider, SkillRegistry};
use async_trait::async_trait;
use nenjo::manifest::AgentManifest;
use nenjo::skills::{SkillProvider, SkillRuntimeState};
use nenjo::{ToolAutonomy, ToolContext, ToolFactory, ToolSecurity};
use nenjo_platform::{
    ManifestAccessPolicy, ManifestMcpBackend,
    tools::{
        PlatformNotificationEmitter, PlatformNotificationToolsBackend, add_manifest_tools,
        add_notification_tools, add_project_rest_tools,
    },
};

use super::native_media::tool_name;
use super::platform_services::PlatformToolServices;
use super::{
    AutonomyLevel, BrowserOpenTool, ContentSearchTool, FileDeleteTool, FileEditTool, FileReadTool,
    FileWriteTool, GitOperationsTool, GlobSearchTool, HttpRequestTool, ListInstalledSkillsTool,
    NativeMediaTool, RuntimeAdapter, ScreenshotTool, SecurityPolicy, ShellTool, SkillMcpTool, Tool,
    UseSkillTool, WebFetchTool, WebSearchTool,
};

tokio::task_local! {
    static PLATFORM_NOTIFICATION_EMITTER: Arc<dyn PlatformNotificationEmitter>;
}

static REGISTERED_PLATFORM_NOTIFICATION_EMITTER: LazyLock<
    RwLock<Option<Arc<dyn PlatformNotificationEmitter>>>,
> = LazyLock::new(|| RwLock::new(None));

/// Process-local notification transport used when tool construction happens in
/// a task spawned outside the Tokio task-local notification scope.
///
/// This is intentionally transport only. The transcript identity for a
/// notification comes from `ToolContext::current_session_id`, which routine
/// execution sets to the stable step run id.
pub(crate) struct PlatformNotificationEmitterRegistration {
    emitter: Arc<dyn PlatformNotificationEmitter>,
}

impl Drop for PlatformNotificationEmitterRegistration {
    fn drop(&mut self) {
        let Ok(mut emitter) = REGISTERED_PLATFORM_NOTIFICATION_EMITTER.write() else {
            return;
        };
        let should_remove = emitter
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, &self.emitter));
        if should_remove {
            *emitter = None;
        }
    }
}

pub(crate) fn register_platform_notification_emitter(
    emitter: Arc<dyn PlatformNotificationEmitter>,
) -> PlatformNotificationEmitterRegistration {
    if let Ok(mut current) = REGISTERED_PLATFORM_NOTIFICATION_EMITTER.write() {
        *current = Some(emitter.clone());
    }
    PlatformNotificationEmitterRegistration { emitter }
}

fn registered_platform_notification_emitter() -> Option<Arc<dyn PlatformNotificationEmitter>> {
    REGISTERED_PLATFORM_NOTIFICATION_EMITTER
        .read()
        .ok()
        .and_then(|emitter| emitter.as_ref().cloned())
}

pub(crate) async fn with_platform_notification_emitter<F, T>(
    emitter: Arc<dyn PlatformNotificationEmitter>,
    future: F,
) -> T
where
    F: Future<Output = T>,
{
    PLATFORM_NOTIFICATION_EMITTER.scope(emitter, future).await
}

/// A tool factory that builds per-agent tool sets for the worker runtime.
///
/// Uses the agent's configuration, security policy, MCP server pool, and
/// manifest backend to build a complete tool set per agent.
pub struct WorkerToolFactory<R>
where
    R: RuntimeAdapter,
{
    security: Arc<SecurityPolicy>,
    runtime: Arc<R>,
    config: Config,
    provider_registry: Arc<ModelProviderRegistry>,
    external_mcp: Arc<ExternalMcpPool>,
    skill_registry: Arc<SkillRegistry>,
    platform: PlatformToolServices,
}

impl<R> WorkerToolFactory<R>
where
    R: RuntimeAdapter + 'static,
{
    #[allow(dead_code)]
    pub(crate) fn new(
        security: impl Into<Arc<SecurityPolicy>>,
        runtime: R,
        config: Config,
        platform: PlatformToolServices,
        external_mcp: Arc<ExternalMcpPool>,
    ) -> Self {
        Self::with_skill_registry(
            security,
            runtime,
            config,
            platform,
            external_mcp,
            Arc::new(SkillRegistry::default()),
        )
    }

    pub(crate) fn with_skill_registry(
        security: impl Into<Arc<SecurityPolicy>>,
        runtime: R,
        config: Config,
        platform: PlatformToolServices,
        external_mcp: Arc<ExternalMcpPool>,
        skill_registry: Arc<SkillRegistry>,
    ) -> Self {
        let provider_registry = Arc::new(ModelProviderRegistry::new(
            &config.model_provider_api_keys,
            &config.reliability,
        ));
        Self::with_skill_registry_and_provider_registry(
            security,
            runtime,
            config,
            provider_registry,
            platform,
            external_mcp,
            skill_registry,
        )
    }

    pub(crate) fn with_skill_registry_and_provider_registry(
        security: impl Into<Arc<SecurityPolicy>>,
        runtime: R,
        config: Config,
        provider_registry: Arc<ModelProviderRegistry>,
        platform: PlatformToolServices,
        external_mcp: Arc<ExternalMcpPool>,
        skill_registry: Arc<SkillRegistry>,
    ) -> Self {
        let security = security.into();
        let runtime = Arc::new(runtime);
        Self {
            security,
            runtime,
            config,
            provider_registry,
            external_mcp,
            skill_registry,
            platform,
        }
    }

    /// Build the base tool set (always included).
    pub fn base_tools(&self) -> Vec<Arc<dyn Tool>> {
        self.base_tools_with(&self.security, Arc::new(SkillRuntimeState::default()))
    }

    /// Build the base tool set with a given security policy.
    fn base_tools_with(
        &self,
        security: &Arc<SecurityPolicy>,
        skill_runtime: Arc<SkillRuntimeState>,
    ) -> Vec<Arc<dyn Tool>> {
        let mut tools: Vec<Arc<dyn Tool>> = vec![
            Arc::new(ShellTool::with_skill_runtime(
                security.clone(),
                self.runtime.clone(),
                skill_runtime.clone(),
            )),
            Arc::new(FileReadTool::new(security.clone())),
            Arc::new(FileWriteTool::new(security.clone())),
            Arc::new(FileEditTool::new(security.clone())),
            Arc::new(FileDeleteTool::new(security.clone())),
            Arc::new(GitOperationsTool::new(security.clone())),
            Arc::new(ContentSearchTool::new(security.clone())),
            Arc::new(GlobSearchTool::new(security.clone())),
        ];
        let skill_provider: Arc<dyn SkillProvider> = Arc::new(LocalSkillProvider::with_mcp_pool(
            self.skill_registry.clone(),
            security.clone(),
            self.external_mcp.clone(),
        ));
        tools.push(Arc::new(UseSkillTool::new(
            skill_provider.clone(),
            skill_runtime.clone(),
        )));
        tools.push(Arc::new(ListInstalledSkillsTool::new(skill_provider)));
        tools.push(Arc::new(SkillMcpTool::new(
            self.external_mcp.clone(),
            skill_runtime,
        )));
        tools
    }

    /// Build all tools for an agent with a given security policy.
    async fn build_tools(
        &self,
        agent: &AgentManifest,
        security: &Arc<SecurityPolicy>,
        tool_context: ToolContext,
    ) -> Vec<Arc<dyn Tool>> {
        let skill_runtime = Arc::new(SkillRuntimeState::default());
        let mut tools = self.base_tools_with(security, skill_runtime);

        // Add MCP tools scoped to this agent's server assignments and platform scopes.
        if !agent.mcp_servers.is_empty() {
            let mcp_tools = self
                .external_mcp
                .tools_for_agent(
                    &agent.mcp_servers,
                    if agent.platform_scopes.is_empty() {
                        None
                    } else {
                        Some(&agent.platform_scopes)
                    },
                )
                .await;
            // Convert Box<dyn Tool> → Arc<dyn Tool>
            for t in mcp_tools {
                tools.push(Arc::from(t));
            }
        }

        let policy = ManifestAccessPolicy::new(agent.platform_scopes.clone());

        let manifest_backend = self.platform.manifest_backend.as_ref().map(|backend| {
            Arc::new(
                backend
                    .as_ref()
                    .clone()
                    .with_current_library_slug(tool_context.project_slug.clone()),
            ) as Arc<dyn ManifestMcpBackend>
        });

        if let Some(backend) = manifest_backend.as_ref() {
            add_manifest_tools(&mut tools, backend.clone(), &policy);
        }

        let project_backend = self.platform.project_backend.clone();
        add_project_rest_tools(&mut tools, project_backend, &policy);

        let notification_sink = PLATFORM_NOTIFICATION_EMITTER
            .try_with(|emitter| emitter.clone())
            .ok()
            .or_else(registered_platform_notification_emitter);
        let notification_backend = self
            .platform
            .platform_client
            .as_ref()
            .zip(self.platform.payload_encoder.as_ref())
            .map(
                |(client, payload_encoder)| PlatformNotificationToolsBackend {
                    client: client.clone(),
                    payload_encoder: payload_encoder.clone(),
                    cached_org_id: self.platform.cached_org_id,
                    agent: agent.slug.clone(),
                    current_session_id: tool_context.current_session_id,
                    notification_sink,
                },
            );
        add_notification_tools(&mut tools, notification_backend, &policy);

        // Web fetch (always included with config, deny-by-default via allowed_hosts)
        if self.config.web_fetch.enabled {
            tools.push(Arc::new(WebFetchTool::new(
                security.clone(),
                self.config.web_fetch.allowed_hosts.clone(),
                self.config.web_fetch.blocked_hosts.clone(),
                self.config.web.allow_private_hosts,
                self.config.web_fetch.max_response_size,
                self.config.web_fetch.timeout_secs,
            )));
        }

        // Web search
        if self.config.web_search.enabled {
            tools.push(Arc::new(WebSearchTool::new(
                self.config.web_search.provider.clone(),
                self.config.web_search.brave_api_key.clone(),
                self.config.web_search.max_results,
                self.config.web_search.timeout_secs,
            )));
        }

        // HTTP request
        if self.config.http_request.enabled {
            tools.push(Arc::new(HttpRequestTool::new(
                security.clone(),
                self.config.http_request.allowed_hosts.clone(),
                self.config.web.allow_private_hosts,
                self.config.http_request.max_response_size,
                self.config.http_request.timeout_secs,
            )));
        }

        // Browser
        if self.config.browser.enabled {
            tools.push(Arc::new(BrowserOpenTool::new(
                security.clone(),
                self.config.browser.allowed_hosts.clone(),
                self.config.web.allow_private_hosts,
            )));
            tools.push(Arc::new(ScreenshotTool::new(security.clone())));
        }

        self.add_native_media_tools(agent, &mut tools);

        tools
    }

    fn add_native_media_tools(&self, agent: &AgentManifest, tools: &mut Vec<Arc<dyn Tool>>) {
        if agent.media.is_empty() {
            return;
        }

        let resolver = MediaProviderResolver::new(
            self.config.media_providers.clone(),
            self.provider_registry.as_ref(),
        );
        let mut tool_names = std::collections::HashSet::new();

        for requirement in &agent.media {
            let capability = requirement.capability();
            let Some(name) = tool_name(capability) else {
                tracing::warn!(
                    capability = ?capability,
                    agent = %agent.slug,
                    "Skipping media capability without a worker tool"
                );
                continue;
            };
            if !tool_names.insert(name) {
                tracing::warn!(
                    capability = ?capability,
                    agent = %agent.slug,
                    "Skipping duplicate media capability assignment"
                );
                continue;
            }

            match resolver.resolve(requirement) {
                Ok(resolved) => {
                    if let Some(tool) =
                        NativeMediaTool::new(resolved, self.provider_registry.clone())
                    {
                        tools.push(Arc::new(tool));
                    }
                }
                Err(error) => {
                    tracing::warn!(
                        capability = ?capability,
                        agent = %agent.slug,
                        error = %error,
                        "Skipping unresolved media capability"
                    );
                }
            }
        }
    }
}

#[async_trait]
impl<R> ToolFactory for WorkerToolFactory<R>
where
    R: RuntimeAdapter + 'static,
{
    async fn create_tools(&self, agent: &AgentManifest) -> Vec<Arc<dyn Tool>> {
        self.build_tools(agent, &self.security, ToolContext::default())
            .await
    }

    async fn create_tools_with_security(
        &self,
        agent: &AgentManifest,
        security: Arc<ToolSecurity>,
    ) -> Vec<Arc<dyn Tool>> {
        let security = Arc::new(security_policy_from_sdk(
            &security,
            &self.security.allowed_runtime_roots,
        ));
        self.build_tools(agent, &security, ToolContext::default())
            .await
    }

    async fn create_tools_with_context(
        &self,
        agent: &AgentManifest,
        security: Arc<ToolSecurity>,
        context: ToolContext,
    ) -> Vec<Arc<dyn Tool>> {
        let security = Arc::new(security_policy_from_sdk(
            &security,
            &self.security.allowed_runtime_roots,
        ));
        self.build_tools(agent, &security, context).await
    }

    fn workspace_dir(&self) -> PathBuf {
        self.security.workspace_dir.clone()
    }
}

fn security_policy_from_sdk(policy: &ToolSecurity, runtime_roots: &[PathBuf]) -> SecurityPolicy {
    let mut security = SecurityPolicy::with_workspace_dir(policy.workspace_dir.clone());
    extend_runtime_roots(&mut security.allowed_runtime_roots, runtime_roots);
    security.autonomy = match policy.autonomy {
        ToolAutonomy::ReadOnly => AutonomyLevel::ReadOnly,
        ToolAutonomy::Supervised => AutonomyLevel::Supervised,
        ToolAutonomy::Full => AutonomyLevel::Full,
    };
    for name in &policy.forwarded_env_names {
        if let Ok(value) = std::env::var(name)
            && !security
                .forwarded_env
                .iter()
                .any(|(existing, _)| existing == name)
        {
            security.forwarded_env.push((name.clone(), value));
        }
    }
    security
}

fn extend_runtime_roots(target: &mut Vec<PathBuf>, roots: &[PathBuf]) {
    for root in roots {
        if !target.iter().any(|existing| existing == root) {
            target.push(root.clone());
        }
    }
}
