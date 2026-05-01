//! Platform-backed manifest backend implementations.

use std::sync::Arc;

use anyhow::{Result, anyhow, bail};
use async_trait::async_trait;
use nenjo::manifest::{
    AbilityManifest, AgentManifest, ContextBlockManifest, CouncilManifest, DomainManifest,
    ManifestResource, ManifestResourceKind, ModelManifest, ProjectManifest, PromptConfig,
    RoutineManifest,
};
use nenjo::{ManifestReader, ManifestWriter};
use uuid::Uuid;

use crate::client::PlatformManifestClient;
use crate::manifest_mcp::*;
use crate::policy::ManifestAccessPolicy;

fn merge_json_patch(target: &mut serde_json::Value, patch: serde_json::Value) {
    match (target, patch) {
        (serde_json::Value::Object(target), serde_json::Value::Object(patch)) => {
            for (key, value) in patch {
                match target.get_mut(&key) {
                    Some(existing) => merge_json_patch(existing, value),
                    None => {
                        target.insert(key, value);
                    }
                }
            }
        }
        (target, patch) => *target = patch,
    }
}

fn merge_prompt_config(current: &PromptConfig, patch: serde_json::Value) -> Result<PromptConfig> {
    let mut value = serde_json::to_value(current)?;
    merge_json_patch(&mut value, patch);
    Ok(serde_json::from_value(value)?)
}

fn local_council_from_document(council: &CouncilDocument) -> CouncilManifest {
    CouncilManifest {
        id: council.summary.id,
        name: council.summary.name.clone(),
        leader_agent_id: council.summary.leader_agent_id,
        members: council
            .members
            .iter()
            .map(|member| nenjo::manifest::CouncilMemberManifest {
                agent_id: member.agent_id,
                agent_name: member.agent_name.clone(),
                priority: member.priority,
            })
            .collect(),
        delegation_strategy: council.summary.delegation_strategy,
    }
}

async fn decode_document_transport_content<E: SensitivePayloadEncoder>(
    codec: &E,
    content: Option<String>,
    encrypted_payload: Option<serde_json::Value>,
) -> Result<String> {
    if let Some(content) = content {
        return Ok(content);
    }

    let payload = encrypted_payload
        .ok_or_else(|| anyhow!("project document content response did not include content"))?;
    let looks_encrypted = payload.get("ciphertext").is_some()
        && payload.get("nonce").is_some()
        && payload.get("object_type").is_some();
    if !looks_encrypted {
        bail!("project document encrypted payload was not in the expected format");
    }
    let Some(decoded) = codec.decode_payload(&payload).await? else {
        bail!("project document content is encrypted and could not be decoded");
    };
    decoded
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| anyhow!("decoded project document content was not a string"))
}

#[async_trait]
/// Encodes and decodes sensitive manifest payloads that should not be sent in plaintext.
pub trait SensitivePayloadEncoder: Send + Sync {
    /// Encode a payload before it is sent to the platform.
    async fn encode_payload(
        &self,
        account_id: Uuid,
        object_id: Uuid,
        object_type: &str,
        payload: &serde_json::Value,
    ) -> Result<Option<serde_json::Value>>;

    /// Decode a payload returned from the platform.
    ///
    /// Returning `Ok(None)` indicates that the caller cannot decode the payload yet.
    async fn decode_payload(
        &self,
        _payload: &serde_json::Value,
    ) -> Result<Option<serde_json::Value>> {
        Ok(None)
    }
}

#[derive(Debug, Clone, Default)]
/// Encoder implementation that leaves sensitive payload handling to the platform.
pub struct NoopSensitivePayloadEncoder;

#[async_trait]
impl SensitivePayloadEncoder for NoopSensitivePayloadEncoder {
    async fn encode_payload(
        &self,
        _account_id: Uuid,
        _object_id: Uuid,
        _object_type: &str,
        _payload: &serde_json::Value,
    ) -> Result<Option<serde_json::Value>> {
        Ok(None)
    }
}

#[derive(Clone)]
/// Manifest MCP backend that serves local state and persists mutations back to the platform API.
pub struct PlatformManifestBackend<L, E> {
    local_store: Arc<L>,
    platform_client: PlatformManifestClient,
    sensitive_payload_encoder: E,
    access_policy: Option<ManifestAccessPolicy>,
}

impl<L, E> PlatformManifestBackend<L, E> {
    /// Create a backend backed by a local manifest store and platform HTTP client.
    pub fn new(
        local_store: Arc<L>,
        platform_client: PlatformManifestClient,
        sensitive_payload_encoder: E,
    ) -> Self {
        Self {
            local_store,
            platform_client,
            sensitive_payload_encoder,
            access_policy: None,
        }
    }

    /// Attach a scope-based access policy used to filter reads and validate writes.
    pub fn with_access_policy(mut self, access_policy: ManifestAccessPolicy) -> Self {
        self.access_policy = Some(access_policy);
        self
    }

    /// Return the local manifest store used for cached reads and write-through updates.
    pub fn local_store(&self) -> &Arc<L> {
        &self.local_store
    }

    /// Return the platform HTTP client used for remote hydration and persistence.
    pub fn platform_client(&self) -> &PlatformManifestClient {
        &self.platform_client
    }
}

impl<L, E> PlatformManifestBackend<L, E>
where
    L: ManifestReader + ManifestWriter + Send + Sync,
    E: SensitivePayloadEncoder,
{
    fn allow_agent(&self, agent: &AgentManifest) -> bool {
        self.access_policy
            .as_ref()
            .map(|policy| policy.allows_agent(agent))
            .unwrap_or(true)
    }

    fn allow_ability(&self, ability: &AbilityManifest) -> bool {
        self.access_policy
            .as_ref()
            .map(|policy| policy.allows_ability(ability))
            .unwrap_or(true)
    }

    fn allow_domain(&self, domain: &DomainManifest) -> bool {
        self.access_policy
            .as_ref()
            .map(|policy| policy.allows_domain(domain))
            .unwrap_or(true)
    }

    fn validate_agent_scopes(&self, scopes: &[String]) -> bool {
        self.access_policy
            .as_ref()
            .map(|policy| policy.validate_agent_scopes(scopes))
            .unwrap_or(true)
    }

    fn validate_ability_scopes(&self, scopes: &[String]) -> bool {
        self.access_policy
            .as_ref()
            .map(|policy| policy.validate_ability_scopes(scopes))
            .unwrap_or(true)
    }

    async fn local_manifest_user_id(&self) -> Result<Uuid> {
        self.local_store
            .load_manifest()
            .await?
            .auth
            .map(|auth| auth.user_id)
            .ok_or_else(|| anyhow!("local manifest is missing auth.user_id"))
    }

    async fn cached_or_remote_ability(&self, id: Uuid) -> Result<AbilityManifest> {
        if let Some(ability) = self.local_store.get_ability(id).await? {
            return Ok(ability);
        }

        let Some(remote) = self.platform_client.fetch_ability_document(id).await? else {
            return Err(anyhow!("ability not found in local manifest: {}", id));
        };

        let hydrated = AbilityManifest {
            id: remote.summary.id,
            name: remote.summary.name,
            tool_name: remote.summary.tool_name,
            path: remote.summary.path,
            display_name: remote.summary.display_name,
            description: remote.summary.description,
            activation_condition: remote.activation_condition,
            prompt_config: Default::default(),
            platform_scopes: remote.platform_scopes,
            mcp_server_ids: remote.mcp_server_ids,
        };
        self.local_store
            .upsert_resource(&ManifestResource::Ability(hydrated.clone()))
            .await?;
        Ok(hydrated)
    }
}

#[async_trait]
impl<L, E> ManifestMcpBackend for PlatformManifestBackend<L, E>
where
    L: ManifestReader + ManifestWriter + Send + Sync,
    E: SensitivePayloadEncoder + Send + Sync,
{
    async fn agents_list(&self) -> Result<AgentsListResult> {
        let agents: Vec<AgentSummary> = self
            .local_store
            .list_agents()
            .await?
            .into_iter()
            .filter(|agent| self.allow_agent(agent))
            .map(|agent| AgentDocument::from(agent).summary)
            .collect();
        Ok(AgentsListResult { agents })
    }

    async fn agents_get(&self, params: AgentsGetParams) -> Result<AgentGetResult> {
        let agent = self
            .local_store
            .get_agent(params.id)
            .await?
            .ok_or_else(|| anyhow!("agent not found in local manifest: {}", params.id))?;
        if !self.allow_agent(&agent) {
            return Err(anyhow!("agent not found in local manifest: {}", params.id));
        }
        Ok(AgentGetResult {
            agent: AgentDocument::from(agent),
        })
    }

    async fn agents_get_prompt(
        &self,
        params: AgentPromptGetParams,
    ) -> Result<AgentPromptGetResult> {
        let agent = self
            .local_store
            .get_agent(params.id)
            .await?
            .ok_or_else(|| anyhow!("agent not found in local manifest: {}", params.id))?;
        if !self.allow_agent(&agent) {
            return Err(anyhow!("agent not found in local manifest: {}", params.id));
        }
        Ok(AgentPromptGetResult {
            agent: AgentPromptDocument::from(agent),
        })
    }

    async fn agents_create(&self, params: AgentCreateParams) -> Result<AgentMutationResult> {
        let requested_scopes = params.data.platform_scopes.clone().unwrap_or_default();
        if !self.validate_agent_scopes(&requested_scopes) {
            return Err(anyhow!("requested agent scopes exceed caller scopes"));
        }

        let create = AgentCreateDocument {
            name: params.data.name,
            description: params.data.description,
            color: params.data.color,
            model_id: params.data.model_id,
            platform_scopes: Some(requested_scopes),
        };

        let created = self.platform_client.create_agent_document(&create).await?;

        let local_agent: AgentManifest = created.clone().into();
        self.local_store
            .upsert_resource(&ManifestResource::Agent(local_agent))
            .await?;

        Ok(AgentMutationResult { agent: created })
    }

    async fn agents_update(&self, params: AgentUpdateParams) -> Result<AgentMutationResult> {
        let existing = self
            .local_store
            .get_agent(params.id)
            .await?
            .ok_or_else(|| anyhow!("agent not found in local manifest: {}", params.id))?;
        if !self.allow_agent(&existing) {
            return Err(anyhow!("agent not found in local manifest: {}", params.id));
        }
        let requested_scopes = params
            .data
            .platform_scopes
            .clone()
            .unwrap_or_else(|| existing.platform_scopes.clone());
        if !self.validate_agent_scopes(&requested_scopes) {
            return Err(anyhow!("requested agent scopes exceed caller scopes"));
        }
        let merged = AgentUpdateDocument {
            name: params.data.name.or_else(|| Some(existing.name.clone())),
            description: Some(
                params
                    .data
                    .description
                    .unwrap_or_else(|| existing.description.clone()),
            ),
            color: Some(params.data.color.unwrap_or_else(|| existing.color.clone())),
            model_id: Some(params.data.model_id.unwrap_or(existing.model_id)),
            platform_scopes: Some(
                params
                    .data
                    .platform_scopes
                    .unwrap_or_else(|| existing.platform_scopes.clone()),
            ),
        };
        let updated = self
            .platform_client
            .update_agent_document(params.id, &merged)
            .await?;

        let mut local_agent: AgentManifest = updated.clone().into();
        local_agent.prompt_config = existing.prompt_config.clone();
        local_agent.heartbeat = existing.heartbeat.clone();
        self.local_store
            .upsert_resource(&ManifestResource::Agent(local_agent))
            .await?;

        Ok(AgentMutationResult { agent: updated })
    }

    async fn agents_update_prompt(
        &self,
        params: AgentPromptUpdateParams,
    ) -> Result<AgentPromptMutationResult> {
        let mut agent = self
            .local_store
            .get_agent(params.id)
            .await?
            .ok_or_else(|| anyhow!("agent not found in local manifest: {}", params.id))?;
        if !self.allow_agent(&agent) {
            return Err(anyhow!("agent not found in local manifest: {}", params.id));
        }
        if agent.prompt_locked {
            return Err(anyhow!("agent prompt is locked: {}", params.id));
        }
        if let Some(prompt_patch) = params.prompt_config {
            agent.prompt_config = merge_prompt_config(&agent.prompt_config, prompt_patch)?;
        }
        let prompt_patch = agent.prompt_config.clone();
        let prompt_payload = serde_json::to_value(&prompt_patch)?;
        let encrypted_payload = self
            .sensitive_payload_encoder
            .encode_payload(
                self.local_manifest_user_id().await?,
                params.id,
                "manifest.agent.prompt",
                &prompt_payload,
            )
            .await?;
        let prompt_config = self
            .platform_client
            .update_agent_prompt_document(params.id, &prompt_payload, encrypted_payload)
            .await?
            .map(serde_json::from_value)
            .transpose()?
            .unwrap_or(prompt_patch);

        agent.prompt_config = prompt_config.clone();
        self.local_store
            .upsert_resource(&ManifestResource::Agent(agent))
            .await?;

        Ok(AgentPromptMutationResult { prompt_config })
    }

    async fn agents_delete(&self, params: AgentDeleteParams) -> Result<DeleteResult> {
        let existing = self
            .local_store
            .get_agent(params.id)
            .await?
            .ok_or_else(|| anyhow!("agent not found in local manifest: {}", params.id))?;
        if !self.allow_agent(&existing) {
            return Err(anyhow!("agent not found in local manifest: {}", params.id));
        }
        self.platform_client
            .delete_agent_document(params.id)
            .await?;
        self.local_store
            .delete_resource(ManifestResourceKind::Agent, params.id)
            .await?;
        Ok(DeleteResult {
            deleted: true,
            id: params.id,
        })
    }

    async fn abilities_list(&self) -> Result<AbilitiesListResult> {
        let abilities: Vec<AbilitySummary> = self
            .local_store
            .list_abilities()
            .await?
            .into_iter()
            .filter(|ability| self.allow_ability(ability))
            .map(|ability| AbilityDocument::from(ability).summary)
            .collect();
        Ok(AbilitiesListResult { abilities })
    }

    async fn abilities_get(&self, params: AbilitiesGetParams) -> Result<AbilityGetResult> {
        let ability = self.cached_or_remote_ability(params.id).await?;
        if !self.allow_ability(&ability) {
            return Err(anyhow!(
                "ability not found in local manifest: {}",
                params.id
            ));
        }
        Ok(AbilityGetResult {
            ability: AbilityDocument::from(ability),
        })
    }

    async fn abilities_get_prompt(
        &self,
        params: AbilityPromptGetParams,
    ) -> Result<AbilityPromptGetResult> {
        let ability = self.cached_or_remote_ability(params.id).await?;
        if !self.allow_ability(&ability) {
            return Err(anyhow!(
                "ability not found in local manifest: {}",
                params.id
            ));
        }
        Ok(AbilityPromptGetResult {
            ability: AbilityPromptDocument::from(ability),
        })
    }

    async fn abilities_create(&self, params: AbilityCreateParams) -> Result<AbilityMutationResult> {
        let requested_scopes = params.data.platform_scopes.clone().unwrap_or_default();
        if !self.validate_ability_scopes(&requested_scopes) {
            return Err(anyhow!("requested ability scopes exceed caller scopes"));
        }
        let encrypted_payload = self
            .sensitive_payload_encoder
            .encode_payload(
                self.local_manifest_user_id().await?,
                Uuid::new_v4(),
                "manifest.ability.prompt",
                &serde_json::json!(params.data.prompt_config.clone()),
            )
            .await?;
        let created = self
            .platform_client
            .create_ability_document(&params.data, encrypted_payload)
            .await?;
        let local_ability = AbilityManifest {
            id: created.summary.id,
            name: created.summary.name.clone(),
            tool_name: created.summary.tool_name.clone(),
            path: created.summary.path.clone(),
            display_name: created.summary.display_name.clone(),
            description: created.summary.description.clone(),
            activation_condition: created.activation_condition.clone(),
            prompt_config: params.data.prompt_config.clone(),
            platform_scopes: created.platform_scopes.clone(),
            mcp_server_ids: created.mcp_server_ids.clone(),
        };
        self.local_store
            .upsert_resource(&ManifestResource::Ability(local_ability))
            .await?;
        Ok(AbilityMutationResult { ability: created })
    }

    async fn abilities_update(&self, params: AbilityUpdateParams) -> Result<AbilityMutationResult> {
        if params.data.is_empty() {
            return Err(anyhow!(
                "ability update requires at least one field in data"
            ));
        }
        let existing = self.cached_or_remote_ability(params.id).await?;
        if !self.allow_ability(&existing) {
            return Err(anyhow!(
                "ability not found in local manifest: {}",
                params.id
            ));
        }
        let requested_scopes = params
            .data
            .platform_scopes
            .clone()
            .unwrap_or_else(|| existing.platform_scopes.clone());
        if !self.validate_ability_scopes(&requested_scopes) {
            return Err(anyhow!("requested ability scopes exceed caller scopes"));
        }
        let merged = AbilityUpdateDocument {
            tool_name: params
                .data
                .tool_name
                .or_else(|| Some(existing.tool_name.clone())),
            display_name: Some(
                params
                    .data
                    .display_name
                    .unwrap_or_else(|| existing.display_name.clone()),
            ),
            description: Some(
                params
                    .data
                    .description
                    .unwrap_or_else(|| existing.description.clone()),
            ),
            activation_condition: params
                .data
                .activation_condition
                .or_else(|| Some(existing.activation_condition.clone())),
            platform_scopes: params
                .data
                .platform_scopes
                .or_else(|| Some(existing.platform_scopes.clone())),
            mcp_server_ids: params
                .data
                .mcp_server_ids
                .or_else(|| Some(existing.mcp_server_ids.clone())),
        };
        let updated = self
            .platform_client
            .update_ability_document(params.id, &merged)
            .await?;
        let local_ability = AbilityManifest {
            id: updated.summary.id,
            name: updated.summary.name.clone(),
            tool_name: updated.summary.tool_name.clone(),
            path: updated.summary.path.clone(),
            display_name: updated.summary.display_name.clone(),
            description: updated.summary.description.clone(),
            activation_condition: updated.activation_condition.clone(),
            prompt_config: existing.prompt_config.clone(),
            platform_scopes: updated.platform_scopes.clone(),
            mcp_server_ids: updated.mcp_server_ids.clone(),
        };
        self.local_store
            .upsert_resource(&ManifestResource::Ability(local_ability))
            .await?;
        Ok(AbilityMutationResult { ability: updated })
    }

    async fn abilities_update_prompt(
        &self,
        params: AbilityPromptUpdateParams,
    ) -> Result<AbilityPromptMutationResult> {
        let existing = self.cached_or_remote_ability(params.id).await?;
        if !self.allow_ability(&existing) {
            return Err(anyhow!(
                "ability not found in local manifest: {}",
                params.id
            ));
        }
        let prompt_config = params.prompt_config;
        let updated = self
            .platform_client
            .update_ability_prompt_document(
                params.id,
                &prompt_config,
                self.sensitive_payload_encoder
                    .encode_payload(
                        self.local_manifest_user_id().await?,
                        params.id,
                        "manifest.ability.prompt",
                        &serde_json::json!(prompt_config.clone()),
                    )
                    .await?,
            )
            .await?;
        let local_ability = AbilityManifest {
            id: existing.id,
            name: existing.name,
            tool_name: existing.tool_name,
            path: existing.path,
            display_name: existing.display_name,
            description: existing.description,
            activation_condition: existing.activation_condition,
            prompt_config: updated.prompt_config.clone(),
            platform_scopes: existing.platform_scopes,
            mcp_server_ids: existing.mcp_server_ids,
        };
        self.local_store
            .upsert_resource(&ManifestResource::Ability(local_ability))
            .await?;
        Ok(AbilityPromptMutationResult {
            prompt_config: updated.prompt_config,
        })
    }

    async fn abilities_delete(&self, params: AbilityDeleteParams) -> Result<DeleteResult> {
        let existing = self.cached_or_remote_ability(params.id).await?;
        if !self.allow_ability(&existing) {
            return Err(anyhow!(
                "ability not found in local manifest: {}",
                params.id
            ));
        }
        self.platform_client
            .delete_ability_document(params.id)
            .await?;
        self.local_store
            .delete_resource(ManifestResourceKind::Ability, params.id)
            .await?;
        Ok(DeleteResult {
            deleted: true,
            id: params.id,
        })
    }

    async fn domains_list(&self) -> Result<DomainsListResult> {
        let domains: Vec<DomainSummary> = self
            .local_store
            .list_domains()
            .await?
            .into_iter()
            .filter(|domain| self.allow_domain(domain))
            .map(|domain| DomainDocument::from(domain).summary)
            .collect();
        Ok(DomainsListResult { domains })
    }

    async fn domains_get(&self, params: DomainsGetParams) -> Result<DomainGetResult> {
        let domain = self
            .local_store
            .get_domain(params.id)
            .await?
            .ok_or_else(|| anyhow!("domain not found in local manifest: {}", params.id))?;
        if !self.allow_domain(&domain) {
            return Err(anyhow!("domain not found in local manifest: {}", params.id));
        }
        Ok(DomainGetResult {
            domain: DomainDocument::from(domain),
        })
    }

    async fn domains_get_manifest(
        &self,
        params: DomainManifestGetParams,
    ) -> Result<DomainManifestGetResult> {
        let domain = self
            .local_store
            .get_domain(params.id)
            .await?
            .ok_or_else(|| anyhow!("domain not found in local manifest: {}", params.id))?;
        if !self.allow_domain(&domain) {
            return Err(anyhow!("domain not found in local manifest: {}", params.id));
        }
        Ok(DomainManifestGetResult {
            domain: DomainManifestDocument::from(domain),
        })
    }

    async fn domains_create(&self, params: DomainCreateParams) -> Result<DomainMutationResult> {
        let encrypted_payload = self
            .sensitive_payload_encoder
            .encode_payload(
                self.local_manifest_user_id().await?,
                Uuid::new_v4(),
                "manifest.domain.prompt",
                &serde_json::json!(params.data.prompt_config.clone()),
            )
            .await?;
        let created = self
            .platform_client
            .create_domain_document(&params.data, encrypted_payload)
            .await?;
        let local_domain = DomainManifest {
            id: created.summary.id,
            name: created.summary.name.clone(),
            path: created.summary.path.clone(),
            display_name: created.summary.display_name.clone(),
            description: created.summary.description.clone(),
            command: created.command.clone(),
            platform_scopes: params.data.platform_scopes.clone().unwrap_or_default(),
            ability_ids: params.data.ability_ids.clone().unwrap_or_default(),
            mcp_server_ids: params.data.mcp_server_ids.clone().unwrap_or_default(),
            prompt_config: params.data.prompt_config.clone().unwrap_or_default(),
        };
        self.local_store
            .upsert_resource(&ManifestResource::Domain(local_domain))
            .await?;
        Ok(DomainMutationResult { domain: created })
    }

    async fn domains_update(&self, params: DomainUpdateParams) -> Result<DomainMutationResult> {
        let existing = self
            .local_store
            .get_domain(params.id)
            .await?
            .ok_or_else(|| anyhow!("domain not found in local manifest: {}", params.id))?;
        if !self.allow_domain(&existing) {
            return Err(anyhow!("domain not found in local manifest: {}", params.id));
        }
        if params.data.is_empty() {
            return Err(anyhow!("domain update requires at least one field"));
        }
        let merged = DomainUpdateDocument {
            name: params.data.name.or_else(|| Some(existing.name.clone())),
            display_name: params
                .data
                .display_name
                .or_else(|| Some(existing.display_name.clone())),
            description: Some(
                params
                    .data
                    .description
                    .unwrap_or_else(|| existing.description.clone()),
            ),
            command: params
                .data
                .command
                .or_else(|| Some(existing.command.clone())),
            platform_scopes: Some(
                params
                    .data
                    .platform_scopes
                    .unwrap_or_else(|| existing.platform_scopes.clone()),
            ),
            ability_ids: Some(
                params
                    .data
                    .ability_ids
                    .unwrap_or_else(|| existing.ability_ids.clone()),
            ),
            mcp_server_ids: Some(
                params
                    .data
                    .mcp_server_ids
                    .unwrap_or_else(|| existing.mcp_server_ids.clone()),
            ),
        };
        if let Some(policy) = &self.access_policy
            && !policy.validate_domain_scopes(merged.platform_scopes.as_deref().unwrap_or(&[]))
        {
            return Err(anyhow!("requested domain scopes exceed caller permissions"));
        }
        let updated = self
            .platform_client
            .update_domain_document(params.id, &merged)
            .await?;
        let local_domain = DomainManifest {
            id: updated.summary.id,
            name: updated.summary.name.clone(),
            path: updated.summary.path.clone(),
            display_name: updated.summary.display_name.clone(),
            description: updated.summary.description.clone(),
            command: updated.command.clone(),
            platform_scopes: updated.platform_scopes.clone(),
            ability_ids: updated.ability_ids.clone(),
            mcp_server_ids: updated.mcp_server_ids.clone(),
            prompt_config: existing.prompt_config.clone(),
        };
        self.local_store
            .upsert_resource(&ManifestResource::Domain(local_domain))
            .await?;
        Ok(DomainMutationResult { domain: updated })
    }

    async fn domains_update_manifest(
        &self,
        params: DomainManifestUpdateParams,
    ) -> Result<DomainManifestMutationResult> {
        let existing = self
            .local_store
            .get_domain(params.id)
            .await?
            .ok_or_else(|| anyhow!("domain not found in local manifest: {}", params.id))?;
        if !self.allow_domain(&existing) {
            return Err(anyhow!("domain not found in local manifest: {}", params.id));
        }
        if let Some(policy) = &self.access_policy
            && !policy.validate_domain_scopes(&existing.platform_scopes)
        {
            return Err(anyhow!("requested domain scopes exceed caller permissions"));
        }
        let updated = self
            .platform_client
            .update_domain_manifest_document(
                params.id,
                params.prompt_config.clone(),
                self.sensitive_payload_encoder
                    .encode_payload(
                        self.local_manifest_user_id().await?,
                        params.id,
                        "manifest.domain.prompt",
                        &serde_json::json!(params.prompt_config.clone()),
                    )
                    .await?,
            )
            .await?;
        let local_domain = DomainManifest {
            id: existing.id,
            name: existing.name.clone(),
            path: existing.path.clone(),
            display_name: existing.display_name.clone(),
            description: existing.description.clone(),
            command: existing.command.clone(),
            platform_scopes: existing.platform_scopes.clone(),
            ability_ids: existing.ability_ids.clone(),
            mcp_server_ids: existing.mcp_server_ids.clone(),
            prompt_config: updated.prompt_config.clone(),
        };
        self.local_store
            .upsert_resource(&ManifestResource::Domain(local_domain))
            .await?;
        Ok(DomainManifestMutationResult {
            prompt_config: updated.prompt_config,
        })
    }

    async fn domains_delete(&self, params: DomainDeleteParams) -> Result<DeleteResult> {
        let existing = self
            .local_store
            .get_domain(params.id)
            .await?
            .ok_or_else(|| anyhow!("domain not found in local manifest: {}", params.id))?;
        if !self.allow_domain(&existing) {
            return Err(anyhow!("domain not found in local manifest: {}", params.id));
        }
        self.platform_client
            .delete_domain_document(params.id)
            .await?;
        self.local_store
            .delete_resource(ManifestResourceKind::Domain, params.id)
            .await?;
        Ok(DeleteResult {
            deleted: true,
            id: params.id,
        })
    }

    async fn projects_list(&self) -> Result<ProjectsListResult> {
        let projects: Vec<ProjectSummary> = self
            .local_store
            .list_projects()
            .await?
            .into_iter()
            .map(|project| ProjectDocument::from(project).summary)
            .collect();
        Ok(ProjectsListResult { projects })
    }

    async fn projects_get(&self, params: ProjectsGetParams) -> Result<ProjectGetResult> {
        let project = self
            .local_store
            .get_project(params.id)
            .await?
            .ok_or_else(|| anyhow!("project not found in local manifest: {}", params.id))?;
        Ok(ProjectGetResult {
            project: ProjectDocument::from(project),
        })
    }

    async fn projects_create(&self, params: ProjectCreateParams) -> Result<ProjectMutationResult> {
        let created = self
            .platform_client
            .create_project_document(&params.data)
            .await?;
        let local_project = ProjectManifest {
            id: created.summary.id,
            name: created.summary.name.clone(),
            slug: created.summary.slug.clone(),
            description: created.summary.description.clone(),
            settings: created.settings.clone(),
        };
        self.local_store
            .upsert_resource(&ManifestResource::Project(local_project))
            .await?;
        Ok(ProjectMutationResult { project: created })
    }

    async fn projects_update(&self, params: ProjectUpdateParams) -> Result<ProjectMutationResult> {
        let existing = self
            .local_store
            .get_project(params.id)
            .await?
            .ok_or_else(|| anyhow!("project not found in local manifest: {}", params.id))?;
        let merged = ProjectUpdateDocument {
            name: params.data.name.or_else(|| Some(existing.name.clone())),
            description: Some(
                params
                    .data
                    .description
                    .unwrap_or_else(|| existing.description.clone()),
            ),
            repo_url: Some(params.data.repo_url.unwrap_or_else(|| {
                existing
                    .settings
                    .get("repo_url")
                    .and_then(|value| value.as_str())
                    .map(ToOwned::to_owned)
            })),
        };
        let updated = self
            .platform_client
            .update_project_document(params.id, &merged)
            .await?;
        let local_project = ProjectManifest {
            id: updated.summary.id,
            name: updated.summary.name.clone(),
            slug: updated.summary.slug.clone(),
            description: updated.summary.description.clone(),
            settings: updated.settings.clone(),
        };
        self.local_store
            .upsert_resource(&ManifestResource::Project(local_project))
            .await?;
        Ok(ProjectMutationResult { project: updated })
    }

    async fn projects_delete(&self, params: ProjectDeleteParams) -> Result<DeleteResult> {
        self.platform_client
            .delete_project_document(params.id)
            .await?;
        self.local_store
            .delete_resource(ManifestResourceKind::Project, params.id)
            .await?;
        Ok(DeleteResult {
            deleted: true,
            id: params.id,
        })
    }

    async fn project_documents_list(
        &self,
        params: ProjectDocumentsListParams,
    ) -> Result<ProjectDocumentsListResult> {
        let project_documents = self
            .platform_client
            .list_project_documents(params.project_id)
            .await?;
        Ok(ProjectDocumentsListResult { project_documents })
    }

    async fn project_documents_get(
        &self,
        params: ProjectDocumentGetParams,
    ) -> Result<ProjectDocumentGetResult> {
        let project_document = self
            .platform_client
            .get_project_document(params.project_id, params.document_id)
            .await?
            .ok_or_else(|| anyhow!("project document not found: {}", params.document_id))?;
        Ok(ProjectDocumentGetResult { project_document })
    }

    async fn project_documents_get_content(
        &self,
        params: ProjectDocumentContentGetParams,
    ) -> Result<ProjectDocumentContentGetResult> {
        let metadata = self
            .platform_client
            .get_project_document(params.project_id, params.document_id)
            .await?;
        let metadata = metadata
            .ok_or_else(|| anyhow!("project document not found: {}", params.document_id))?;
        let transport = self
            .platform_client
            .fetch_project_document_content(params.project_id, params.document_id)
            .await?;
        let description = decode_document_transport_content(
            &self.sensitive_payload_encoder,
            transport.content,
            transport.encrypted_payload,
        )
        .await?;
        let project_document = ProjectDocumentContentDocument {
            document: ProjectDocumentSummary {
                id: metadata.id,
                project_id: metadata.project_id,
                filename: transport.filename,
                content_type: transport.content_type,
                size_bytes: transport.size_bytes,
                updated_at: metadata.updated_at,
            },
            description,
        };
        Ok(ProjectDocumentContentGetResult { project_document })
    }

    async fn project_documents_create(
        &self,
        params: ProjectDocumentCreateParams,
    ) -> Result<ProjectDocumentMutationResult> {
        let encrypted_payload = self
            .sensitive_payload_encoder
            .encode_payload(
                self.local_manifest_user_id().await?,
                Uuid::new_v4(),
                "manifest.document.content",
                &serde_json::Value::String(params.data.description.clone()),
            )
            .await?;
        let project_document = self
            .platform_client
            .create_project_file_document(&params.data, encrypted_payload)
            .await?;
        Ok(ProjectDocumentMutationResult { project_document })
    }

    async fn project_documents_update_content(
        &self,
        params: ProjectDocumentContentUpdateParams,
    ) -> Result<ProjectDocumentContentMutationResult> {
        let encrypted_payload = self
            .sensitive_payload_encoder
            .encode_payload(
                self.local_manifest_user_id().await?,
                params.document_id,
                "manifest.document.content",
                &serde_json::Value::String(params.description.clone()),
            )
            .await?;
        let project_document = self
            .platform_client
            .update_project_document_content(
                params.project_id,
                params.document_id,
                &params.description,
                encrypted_payload,
            )
            .await?;
        Ok(ProjectDocumentContentMutationResult { project_document })
    }

    async fn project_documents_delete(
        &self,
        params: ProjectDocumentDeleteParams,
    ) -> Result<DeleteResult> {
        self.platform_client
            .delete_project_file_document(params.project_id, params.document_id)
            .await?;
        Ok(DeleteResult {
            deleted: true,
            id: params.document_id,
        })
    }

    async fn routines_list(&self) -> Result<RoutinesListResult> {
        let routines = self
            .local_store
            .list_routines()
            .await?
            .into_iter()
            .map(|routine| RoutineDocument::from(routine).summary)
            .collect();
        Ok(RoutinesListResult { routines })
    }

    async fn routines_get(&self, params: RoutinesGetParams) -> Result<RoutineGetResult> {
        let routine = self
            .local_store
            .get_routine(params.id)
            .await?
            .ok_or_else(|| anyhow!("routine not found in local manifest: {}", params.id))?;
        Ok(RoutineGetResult {
            routine: RoutineDocument::from(routine),
        })
    }

    async fn routines_create(&self, params: RoutineCreateParams) -> Result<RoutineMutationResult> {
        let created = self
            .platform_client
            .create_routine_document(&params.data)
            .await?;
        let local_routine = RoutineManifest {
            id: created.summary.id,
            name: created.summary.name.clone(),
            description: created.summary.description.clone(),
            trigger: created.summary.trigger,
            steps: created.steps.clone(),
            edges: created.edges.clone(),
            metadata: created.metadata.clone(),
        };
        self.local_store
            .upsert_resource(&ManifestResource::Routine(local_routine))
            .await?;
        Ok(RoutineMutationResult { routine: created })
    }

    async fn routines_update(&self, params: RoutineUpdateParams) -> Result<RoutineMutationResult> {
        if params.data.is_empty() {
            return Err(anyhow!(
                "routine update requires at least one field in data"
            ));
        }
        let existing = self
            .local_store
            .get_routine(params.id)
            .await?
            .ok_or_else(|| anyhow!("routine not found in local manifest: {}", params.id))?;
        let merged = RoutineUpdateDocument {
            name: params.data.name.or_else(|| Some(existing.name.clone())),
            description: Some(
                params
                    .data
                    .description
                    .unwrap_or_else(|| existing.description.clone()),
            ),
            trigger: params.data.trigger.or(Some(existing.trigger)),
            metadata: params
                .data
                .metadata
                .or_else(|| Some(existing.metadata.clone())),
            graph: params
                .data
                .graph
                .or_else(|| Some(RoutineDocument::from(existing).graph_input())),
        };
        let updated = self
            .platform_client
            .update_routine_document(params.id, &merged)
            .await?;
        let local_routine = RoutineManifest {
            id: updated.summary.id,
            name: updated.summary.name.clone(),
            description: updated.summary.description.clone(),
            trigger: updated.summary.trigger,
            steps: updated.steps.clone(),
            edges: updated.edges.clone(),
            metadata: updated.metadata.clone(),
        };
        self.local_store
            .upsert_resource(&ManifestResource::Routine(local_routine))
            .await?;
        Ok(RoutineMutationResult { routine: updated })
    }

    async fn routines_delete(&self, params: RoutineDeleteParams) -> Result<DeleteResult> {
        self.platform_client
            .delete_routine_document(params.id)
            .await?;
        self.local_store
            .delete_resource(ManifestResourceKind::Routine, params.id)
            .await?;
        Ok(DeleteResult {
            deleted: true,
            id: params.id,
        })
    }

    async fn models_list(&self) -> Result<ModelsListResult> {
        let models = self
            .local_store
            .list_models()
            .await?
            .into_iter()
            .map(|model| ModelDocument::from(model).summary)
            .collect();
        Ok(ModelsListResult { models })
    }

    async fn models_get(&self, params: ModelsGetParams) -> Result<ModelGetResult> {
        let model = self
            .local_store
            .get_model(params.id)
            .await?
            .ok_or_else(|| anyhow!("model not found in local manifest: {}", params.id))?;
        Ok(ModelGetResult {
            model: ModelDocument::from(model),
        })
    }

    async fn models_create(&self, params: ModelCreateParams) -> Result<ModelMutationResult> {
        let created = self
            .platform_client
            .create_model_document(&params.data)
            .await?;
        let local_model = ModelManifest {
            id: created.summary.id,
            name: created.summary.name.clone(),
            description: created.summary.description.clone(),
            model: created.summary.model.clone(),
            model_provider: created.summary.model_provider.clone(),
            temperature: created.temperature,
            base_url: created.base_url.clone(),
        };
        self.local_store
            .upsert_resource(&ManifestResource::Model(local_model))
            .await?;
        Ok(ModelMutationResult { model: created })
    }

    async fn models_update(&self, params: ModelUpdateParams) -> Result<ModelMutationResult> {
        let existing = self
            .local_store
            .get_model(params.id)
            .await?
            .ok_or_else(|| anyhow!("model not found in local manifest: {}", params.id))?;
        let merged = ModelUpdateDocument {
            name: params.data.name.or_else(|| Some(existing.name.clone())),
            description: Some(
                params
                    .data
                    .description
                    .unwrap_or_else(|| existing.description.clone()),
            ),
            model: params.data.model.or_else(|| Some(existing.model.clone())),
            model_provider: params
                .data
                .model_provider
                .or_else(|| Some(existing.model_provider.clone())),
            temperature: params.data.temperature.or(existing.temperature),
            base_url: Some(params.data.base_url.unwrap_or(existing.base_url.clone())),
        };
        let updated = self
            .platform_client
            .update_model_document(params.id, &merged)
            .await?;
        let local_model = ModelManifest {
            id: updated.summary.id,
            name: updated.summary.name.clone(),
            description: updated.summary.description.clone(),
            model: updated.summary.model.clone(),
            model_provider: updated.summary.model_provider.clone(),
            temperature: updated.temperature,
            base_url: updated.base_url.clone(),
        };
        self.local_store
            .upsert_resource(&ManifestResource::Model(local_model))
            .await?;
        Ok(ModelMutationResult { model: updated })
    }

    async fn models_delete(&self, params: ModelDeleteParams) -> Result<DeleteResult> {
        self.platform_client
            .delete_model_document(params.id)
            .await?;
        self.local_store
            .delete_resource(ManifestResourceKind::Model, params.id)
            .await?;
        Ok(DeleteResult {
            deleted: true,
            id: params.id,
        })
    }

    async fn councils_list(&self) -> Result<CouncilsListResult> {
        let councils = self
            .local_store
            .list_councils()
            .await?
            .into_iter()
            .map(|council| CouncilDocument::from(council).summary)
            .collect();
        Ok(CouncilsListResult { councils })
    }

    async fn councils_get(&self, params: CouncilsGetParams) -> Result<CouncilGetResult> {
        let council = self
            .local_store
            .get_council(params.id)
            .await?
            .ok_or_else(|| anyhow!("council not found in local manifest: {}", params.id))?;
        Ok(CouncilGetResult {
            council: CouncilDocument::from(council),
        })
    }

    async fn councils_create(&self, params: CouncilCreateParams) -> Result<CouncilMutationResult> {
        let created = self
            .platform_client
            .create_council_document(&params.data)
            .await?;
        let local_council = local_council_from_document(&created);
        self.local_store
            .upsert_resource(&ManifestResource::Council(local_council))
            .await?;
        Ok(CouncilMutationResult { council: created })
    }

    async fn councils_update(&self, params: CouncilUpdateParams) -> Result<CouncilMutationResult> {
        let existing = self
            .local_store
            .get_council(params.id)
            .await?
            .ok_or_else(|| anyhow!("council not found in local manifest: {}", params.id))?;
        let merged = CouncilUpdateDocument {
            name: params.data.name.or_else(|| Some(existing.name.clone())),
            description: params.data.description,
            delegation_strategy: params
                .data
                .delegation_strategy
                .or(Some(existing.delegation_strategy)),
            config: params.data.config,
        };
        let updated = self
            .platform_client
            .update_council_document(params.id, &merged)
            .await?;
        let local_council = local_council_from_document(&updated);
        self.local_store
            .upsert_resource(&ManifestResource::Council(local_council))
            .await?;
        Ok(CouncilMutationResult { council: updated })
    }

    async fn councils_add_member(
        &self,
        params: CouncilAddMemberParams,
    ) -> Result<CouncilMutationResult> {
        let updated = self
            .platform_client
            .add_council_member_document(params.council_id, &params.data)
            .await?;
        let local_council = local_council_from_document(&updated);
        self.local_store
            .upsert_resource(&ManifestResource::Council(local_council))
            .await?;
        Ok(CouncilMutationResult { council: updated })
    }

    async fn councils_update_member(
        &self,
        params: CouncilUpdateMemberParams,
    ) -> Result<CouncilMutationResult> {
        if params.data.is_empty() {
            bail!("council member update requires at least one field");
        }
        let updated = self
            .platform_client
            .update_council_member_document(params.council_id, params.agent_id, &params.data)
            .await?;
        let local_council = local_council_from_document(&updated);
        self.local_store
            .upsert_resource(&ManifestResource::Council(local_council))
            .await?;
        Ok(CouncilMutationResult { council: updated })
    }

    async fn councils_remove_member(
        &self,
        params: CouncilRemoveMemberParams,
    ) -> Result<CouncilMutationResult> {
        let updated = self
            .platform_client
            .remove_council_member_document(params.council_id, params.agent_id)
            .await?;
        let local_council = local_council_from_document(&updated);
        self.local_store
            .upsert_resource(&ManifestResource::Council(local_council))
            .await?;
        Ok(CouncilMutationResult { council: updated })
    }

    async fn councils_delete(&self, params: CouncilDeleteParams) -> Result<DeleteResult> {
        self.platform_client
            .delete_council_document(params.id)
            .await?;
        self.local_store
            .delete_resource(ManifestResourceKind::Council, params.id)
            .await?;
        Ok(DeleteResult {
            deleted: true,
            id: params.id,
        })
    }

    async fn context_blocks_list(&self) -> Result<ContextBlocksListResult> {
        let context_blocks: Vec<ContextBlockSummary> = self
            .local_store
            .list_context_blocks()
            .await?
            .into_iter()
            .map(|context_block| ContextBlockDocument::from(context_block).summary)
            .collect();
        Ok(ContextBlocksListResult { context_blocks })
    }

    async fn context_blocks_get(
        &self,
        params: ContextBlocksGetParams,
    ) -> Result<ContextBlockGetResult> {
        let context_block = self
            .local_store
            .get_context_block(params.id)
            .await?
            .ok_or_else(|| anyhow!("context block not found in local manifest: {}", params.id))?;
        Ok(ContextBlockGetResult {
            context_block: ContextBlockDocument::from(context_block),
        })
    }

    async fn context_blocks_get_content(
        &self,
        params: ContextBlockContentGetParams,
    ) -> Result<ContextBlockContentGetResult> {
        let context_block = self
            .local_store
            .get_context_block(params.id)
            .await?
            .ok_or_else(|| anyhow!("context block not found in local manifest: {}", params.id))?;
        Ok(ContextBlockContentGetResult {
            context_block: ContextBlockContentDocument::from(context_block),
        })
    }

    async fn context_blocks_create(
        &self,
        params: ContextBlockCreateParams,
    ) -> Result<ContextBlockMutationResult> {
        let encrypted_payload = self
            .sensitive_payload_encoder
            .encode_payload(
                self.local_manifest_user_id().await?,
                Uuid::new_v4(),
                "manifest.context_block.content",
                &serde_json::json!(params.data.template.clone()),
            )
            .await?;
        let created = self
            .platform_client
            .create_context_block_document(&params.data, encrypted_payload)
            .await?;
        let local_context_block = ContextBlockManifest {
            id: created.summary.id,
            name: created.summary.name.clone(),
            path: created.summary.path.clone(),
            display_name: created.summary.display_name.clone(),
            description: created.summary.description.clone(),
            template: params.data.template.clone(),
        };
        self.local_store
            .upsert_resource(&ManifestResource::ContextBlock(local_context_block))
            .await?;
        Ok(ContextBlockMutationResult {
            context_block: created,
        })
    }

    async fn context_blocks_update(
        &self,
        params: ContextBlockUpdateParams,
    ) -> Result<ContextBlockMutationResult> {
        let existing = self
            .local_store
            .get_context_block(params.id)
            .await?
            .ok_or_else(|| anyhow!("context block not found in local manifest: {}", params.id))?;
        let merged = ContextBlockUpdateDocument {
            name: params.data.name.or_else(|| Some(existing.name.clone())),
            display_name: Some(
                params
                    .data
                    .display_name
                    .unwrap_or_else(|| existing.display_name.clone()),
            ),
            description: Some(
                params
                    .data
                    .description
                    .unwrap_or_else(|| existing.description.clone()),
            ),
            template: None,
        };
        let updated = self
            .platform_client
            .update_context_block_document(params.id, &merged)
            .await?;
        let local_context_block = ContextBlockManifest {
            id: updated.summary.id,
            name: updated.summary.name.clone(),
            path: updated.summary.path.clone(),
            display_name: updated.summary.display_name.clone(),
            description: updated.summary.description.clone(),
            template: existing.template.clone(),
        };
        self.local_store
            .upsert_resource(&ManifestResource::ContextBlock(local_context_block))
            .await?;
        Ok(ContextBlockMutationResult {
            context_block: updated,
        })
    }

    async fn context_blocks_update_content(
        &self,
        params: ContextBlockContentUpdateParams,
    ) -> Result<ContextBlockContentMutationResult> {
        let existing = self
            .local_store
            .get_context_block(params.id)
            .await?
            .ok_or_else(|| anyhow!("context block not found in local manifest: {}", params.id))?;
        let template = params.template.unwrap_or_else(|| existing.template.clone());
        let updated = self
            .platform_client
            .update_context_block_content_document(
                params.id,
                &template,
                self.sensitive_payload_encoder
                    .encode_payload(
                        self.local_manifest_user_id().await?,
                        params.id,
                        "manifest.context_block.content",
                        &serde_json::json!(template.clone()),
                    )
                    .await?,
            )
            .await?;
        let local_context_block = ContextBlockManifest {
            id: existing.id,
            name: existing.name.clone(),
            path: existing.path.clone(),
            display_name: existing.display_name.clone(),
            description: existing.description.clone(),
            template: updated.template.clone(),
        };
        self.local_store
            .upsert_resource(&ManifestResource::ContextBlock(local_context_block))
            .await?;
        Ok(ContextBlockContentMutationResult {
            template: updated.template,
        })
    }

    async fn context_blocks_delete(
        &self,
        params: ContextBlockDeleteParams,
    ) -> Result<DeleteResult> {
        self.platform_client
            .delete_context_block_document(params.id)
            .await?;
        self.local_store
            .delete_resource(ManifestResourceKind::ContextBlock, params.id)
            .await?;
        Ok(DeleteResult {
            deleted: true,
            id: params.id,
        })
    }
}
