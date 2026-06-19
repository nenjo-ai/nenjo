//! Canonical manifest MCP document types.

use derive_builder::Builder;
use nenjo::Slug;
use nenjo_models::NativeModelToolId;
use serde::{Deserialize, Deserializer, Serialize};

use nenjo::manifest::{
    AbilityManifest, AgentHeartbeatManifest, AgentManifest, CommandManifest, ContextBlockManifest,
    CouncilDelegationStrategy, CouncilManifest, DomainManifest, HasManifestSlug, ModelManifest,
    ProjectManifest, PromptConfig, RoutineEdgeCondition, RoutineEdgeManifest, RoutineManifest,
    RoutineMetadata, RoutineStepManifest, RoutineStepType, RoutineTrigger,
};
use nenjo::manifest::{AbilityPromptConfig, DomainPromptConfig};

/// Canonical prompt-free agent document used by manifest list/get/update/delete operations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSummary {
    pub name: String,
    pub slug: Slug,
    pub description: Option<String>,
    pub color: Option<String>,
    #[serde(default)]
    pub model: Option<Slug>,
}

/// Canonical agent document used by manifest get operations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentDocument {
    #[serde(flatten)]
    pub summary: AgentSummary,
    #[serde(default)]
    pub prompt_config: PromptConfig,
    #[serde(default)]
    pub domains: Vec<Slug>,
    #[serde(default)]
    pub platform_scopes: Vec<String>,
    #[serde(default)]
    pub mcp_servers: Vec<Slug>,
    #[serde(default)]
    pub script_tools: Vec<Slug>,
    #[serde(default)]
    pub abilities: Vec<String>,
    #[serde(default)]
    pub prompt_locked: bool,
    #[serde(default)]
    pub heartbeat: Option<AgentHeartbeatManifest>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
/// Metadata patch for `configure_agent`.
pub struct AgentConfigureMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_slug_field",
        skip_serializing_if = "Option::is_none"
    )]
    pub model: Option<Option<Slug>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
/// Full replacement assignment lists for `configure_agent`.
pub struct AgentConfigureAssignments {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abilities: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domains: Option<Vec<Slug>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_servers: Option<Vec<Slug>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
/// Request body for configuring an agent in one backend-owned sequence.
pub struct AgentConfigureDocument {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<uuid::Uuid>,
    #[serde(default)]
    pub agent: Option<Slug>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<AgentConfigureMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_config: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encrypted_payload: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignments: Option<AgentConfigureAssignments>,
}

fn deserialize_optional_slug_field<'de, D>(
    deserializer: D,
) -> Result<Option<Option<Slug>>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<Slug>::deserialize(deserializer).map(Some)
}

impl From<AgentManifest> for AgentDocument {
    fn from(agent: AgentManifest) -> Self {
        Self {
            summary: AgentSummary {
                name: agent.name,
                slug: agent.slug,
                description: agent.description,
                color: agent.color,
                model: agent.model,
            },
            prompt_config: agent.prompt_config,
            domains: agent.domains,
            platform_scopes: agent.platform_scopes,
            mcp_servers: agent.mcp_servers,
            script_tools: agent.script_tools,
            abilities: agent.abilities,
            prompt_locked: agent.prompt_locked,
            heartbeat: agent.heartbeat,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Prompt-free ability metadata returned by list/get operations.
pub struct AbilitySummary {
    pub name: String,
    #[serde(default)]
    pub path: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Canonical ability document returned by get/configure operations.
pub struct AbilityDocument {
    #[serde(flatten)]
    pub summary: AbilitySummary,
    pub activation_condition: String,
    #[serde(default)]
    pub prompt_config: AbilityPromptConfig,
    #[serde(default)]
    pub platform_scopes: Vec<String>,
    #[serde(default)]
    pub mcp_servers: Vec<Slug>,
    #[serde(default)]
    pub script_tools: Vec<Slug>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
/// Metadata patch for `configure_ability`.
pub struct AbilityConfigureMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activation_condition: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
/// Full replacement assignment lists for `configure_ability`.
pub struct AbilityConfigureAssignments {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_servers: Option<Vec<Slug>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script_tools: Option<Vec<Slug>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
/// Request body for configuring an ability in one backend-owned sequence.
pub struct AbilityConfigureDocument {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<uuid::Uuid>,
    #[serde(default)]
    pub ability: Option<Slug>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<AbilityConfigureMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_config: Option<AbilityPromptConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encrypted_payload: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignments: Option<AbilityConfigureAssignments>,
}

impl From<AbilityManifest> for AbilityDocument {
    fn from(ability: AbilityManifest) -> Self {
        Self {
            summary: AbilitySummary {
                name: ability.name,
                path: ability.path.unwrap_or_default(),
                description: ability.description,
            },
            activation_condition: ability.activation_condition,
            prompt_config: ability.prompt_config,
            platform_scopes: ability.platform_scopes,
            mcp_servers: ability.mcp_servers,
            script_tools: ability.script_tools,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
/// Metadata patch for `configure_command`.
pub struct CommandConfigureMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Option<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Content-free command metadata returned by list operations.
pub struct CommandSummary {
    pub name: String,
    pub slug: Slug,
    #[serde(default)]
    pub path: String,
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub hooks: Vec<Slug>,
    #[serde(default)]
    pub source_type: String,
    #[serde(default)]
    pub read_only: bool,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

impl From<CommandManifest> for CommandSummary {
    fn from(command: CommandManifest) -> Self {
        Self {
            slug: command.manifest_slug(),
            name: command.name,
            path: command.path,
            command: command.command,
            display_name: command.display_name,
            description: command.description,
            hooks: command.hooks,
            source_type: command.source_type,
            read_only: command.read_only,
            metadata: command.metadata,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
/// Request body for configuring a command in one backend-owned sequence.
pub struct CommandConfigureDocument {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<uuid::Uuid>,
    #[serde(default)]
    pub command_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<CommandConfigureMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encrypted_payload: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Prompt-free domain metadata returned by list/get operations.
pub struct DomainSummary {
    pub name: String,
    pub slug: Slug,
    #[serde(default)]
    pub path: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Canonical domain document returned by get/configure operations.
pub struct DomainDocument {
    #[serde(flatten)]
    pub summary: DomainSummary,
    pub command: String,
    #[serde(default)]
    pub prompt_config: DomainPromptConfig,
    #[serde(default)]
    pub platform_scopes: Vec<String>,
    #[serde(default)]
    pub abilities: Vec<String>,
    #[serde(default)]
    pub mcp_servers: Vec<Slug>,
    #[serde(default)]
    pub script_tools: Vec<Slug>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
/// Metadata patch for `configure_domain`.
pub struct DomainConfigureMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
/// Full replacement assignment lists for `configure_domain`.
pub struct DomainConfigureAssignments {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abilities: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_servers: Option<Vec<Slug>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script_tools: Option<Vec<Slug>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
/// Request body for configuring a domain in one backend-owned sequence.
pub struct DomainConfigureDocument {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<uuid::Uuid>,
    #[serde(default)]
    pub domain: Option<Slug>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<DomainConfigureMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_config: Option<DomainPromptConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encrypted_payload: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignments: Option<DomainConfigureAssignments>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Prompt-free context block metadata returned by list/get operations.
pub struct ContextBlockSummary {
    pub name: String,
    pub slug: Slug,
    pub selector: String,
    #[serde(default)]
    pub path: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Canonical context block document returned by get/configure operations.
pub struct ContextBlockDocument {
    #[serde(flatten)]
    pub summary: ContextBlockSummary,
    #[serde(default)]
    pub template: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
/// Metadata patch for `configure_context_block`.
pub struct ContextBlockConfigureMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Option<String>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
/// Request body for configuring a context block in one backend-owned sequence.
pub struct ContextBlockConfigureDocument {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<uuid::Uuid>,
    #[serde(default)]
    pub context_block: Option<Slug>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<ContextBlockConfigureMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encrypted_payload: Option<serde_json::Value>,
}

impl From<DomainManifest> for DomainDocument {
    fn from(domain: DomainManifest) -> Self {
        let slug = domain.slug();
        Self {
            summary: DomainSummary {
                name: domain.name,
                slug,
                path: domain.path,
                description: domain.description,
            },
            command: domain.command,
            prompt_config: domain.prompt_config,
            platform_scopes: domain.platform_scopes,
            abilities: domain.abilities,
            mcp_servers: domain.mcp_servers,
            script_tools: domain.script_tools,
        }
    }
}

impl From<ContextBlockManifest> for ContextBlockDocument {
    fn from(context_block: ContextBlockManifest) -> Self {
        let slug = context_block.slug();
        let selector = format!(
            "{{{{ {} }}}}",
            context_block_selector(&context_block.path, &context_block.name)
        );
        Self {
            template: context_block.template,
            summary: ContextBlockSummary {
                name: context_block.name,
                slug,
                selector,
                path: context_block.path,
                description: context_block.description,
            },
        }
    }
}

fn context_block_selector(path: &str, name: &str) -> String {
    if path.trim().is_empty() {
        name.to_string()
    } else {
        let path = path
            .split('/')
            .filter(|part| !part.trim().is_empty())
            .collect::<Vec<_>>()
            .join(".");
        format!("{path}.{name}")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Project metadata returned by list/get operations.
pub struct ProjectSummary {
    pub name: String,
    pub slug: Slug,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Project manifest resource including settings.
pub struct ProjectDocument {
    #[serde(flatten)]
    pub summary: ProjectSummary,
    #[serde(default)]
    pub settings: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Request body for creating a project.
pub struct ProjectCreateDocument {
    pub name: String,
    pub slug: Slug,
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_url: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
/// Partial update body for a project.
pub struct ProjectUpdateDocument {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slug: Option<Slug>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_url: Option<Option<String>>,
}

/// Library knowledge pack metadata returned by pack routes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgePackDocument {
    pub slug: Slug,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub source_type: String,
    pub read_only: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selector: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Builder)]
#[builder(pattern = "owned")]
/// Request body for creating a user-managed Library knowledge pack.
pub struct KnowledgePackCreateDocument {
    pub name: String,
    #[builder(default)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slug: Option<Slug>,
    #[builder(default)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, Builder)]
#[builder(pattern = "owned")]
/// Partial update body for a user-managed Library knowledge pack.
pub struct KnowledgePackUpdateDocument {
    #[builder(default)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[builder(default)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slug: Option<Slug>,
    #[builder(default)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Option<String>>,
}

/// Library knowledge document metadata returned by knowledge pack routes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeDocSummary {
    pub pack: Slug,
    pub slug: Slug,
    pub filename: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    pub content_type: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Library knowledge document including inline content.
pub struct KnowledgeDocContentDocument {
    #[serde(flatten)]
    pub doc: KnowledgeDocSummary,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Builder)]
#[builder(pattern = "owned")]
/// Request body for creating a library knowledge document.
pub struct KnowledgeDocCreateDocument {
    #[serde(
        deserialize_with = "crate::manifest_mcp::serde_helpers::deserialize_library_pack_slug"
    )]
    pub pack: Slug,
    pub filename: String,
    pub content: String,
    #[builder(default)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    #[builder(default)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[builder(default)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[builder(default)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[builder(default)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[builder(default)]
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[builder(default)]
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub related: Vec<KnowledgeDocRelatedDocument>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Builder)]
#[builder(pattern = "owned")]
/// Outbound relationship authored on a library knowledge document.
pub struct KnowledgeDocRelatedDocument {
    /// Stable document slug, selector, or search metadata path accepted at authoring time.
    pub target_doc: String,
    #[serde(rename = "type")]
    pub edge_type: String,
    #[builder(default)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Builder)]
#[builder(pattern = "owned")]
/// Partial update body for library knowledge document content, metadata, and graph edges.
pub struct KnowledgeDocUpdateDocument {
    #[builder(default)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    #[builder(default)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[builder(default)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<Option<String>>,
    #[builder(default)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<Option<String>>,
    #[builder(default)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<Option<String>>,
    #[builder(default)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<Option<String>>,
    #[builder(default)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    #[builder(default)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub related: Option<Vec<KnowledgeDocRelatedDocument>>,
}

impl From<ProjectManifest> for ProjectDocument {
    fn from(project: ProjectManifest) -> Self {
        Self {
            summary: ProjectSummary {
                name: project.name,
                slug: project.slug,
                description: project.description,
            },
            settings: project.settings,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Routine metadata returned by list/get operations.
pub struct RoutineSummary {
    pub slug: Slug,
    pub name: String,
    pub description: Option<String>,
    pub trigger: RoutineTrigger,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// One routine step returned by list/get operations.
pub struct RoutineStepDocument {
    pub slug: Slug,
    pub routine: Slug,
    pub name: String,
    pub step_type: RoutineStepType,
    #[serde(default)]
    pub council: Option<Slug>,
    #[serde(default)]
    pub agent: Option<Slug>,
    #[serde(default)]
    pub config: serde_json::Value,
    pub order_index: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// One routine edge returned by list/get operations.
pub struct RoutineEdgeDocument {
    pub routine: Slug,
    pub source_step: Slug,
    pub target_step: Slug,
    pub condition: RoutineEdgeCondition,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Routine document including steps and edges.
pub struct RoutineDocument {
    #[serde(flatten)]
    pub summary: RoutineSummary,
    #[serde(default)]
    pub metadata: RoutineMetadata,
    #[serde(default)]
    pub steps: Vec<RoutineStepDocument>,
    #[serde(default)]
    pub edges: Vec<RoutineEdgeDocument>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
/// Full graph payload used when creating or replacing a routine workflow.
pub struct RoutineGraphInput {
    #[serde(default)]
    pub entry_steps: Vec<Slug>,
    #[serde(default)]
    pub steps: Vec<RoutineStepInput>,
    #[serde(default)]
    pub edges: Vec<RoutineEdgeInput>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// One step in a routine graph write request.
pub struct RoutineStepInput {
    pub slug: Slug,
    pub name: String,
    pub step_type: RoutineStepType,
    #[serde(default)]
    pub council: Option<Slug>,
    #[serde(default)]
    pub agent: Option<Slug>,
    #[serde(default)]
    pub config: serde_json::Value,
    pub order_index: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// One edge in a routine graph write request.
pub struct RoutineEdgeInput {
    pub source_step: Slug,
    pub target_step: Slug,
    pub condition: RoutineEdgeCondition,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
/// Metadata patch for `configure_routine`.
pub struct RoutineConfigureMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<Option<uuid::Uuid>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger: Option<RoutineTrigger>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_active: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_retries: Option<i32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
/// Request body for configuring a routine in one backend-owned sequence.
pub struct RoutineConfigureDocument {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<uuid::Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routine: Option<Slug>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<RoutineConfigureMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_metadata: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encrypted_payload: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph: Option<RoutineGraphInput>,
}

impl RoutineDocument {
    /// Convert the stored routine document into a graph write payload.
    pub fn graph_input(&self) -> RoutineGraphInput {
        RoutineGraphInput {
            entry_steps: self.metadata.entry_steps.clone(),
            steps: self
                .steps
                .iter()
                .map(|step| RoutineStepInput {
                    slug: step.slug.clone(),
                    name: step.name.clone(),
                    step_type: step.step_type,
                    council: step.council.clone(),
                    agent: step.agent.clone(),
                    config: step.config.clone(),
                    order_index: step.order_index,
                })
                .collect(),
            edges: self
                .edges
                .iter()
                .map(|edge| RoutineEdgeInput {
                    source_step: edge.source_step.clone(),
                    target_step: edge.target_step.clone(),
                    condition: edge.condition,
                    metadata: edge.metadata.clone(),
                })
                .collect(),
        }
    }
}

impl From<RoutineManifest> for RoutineDocument {
    fn from(routine: RoutineManifest) -> Self {
        let slug = routine.slug().clone();
        Self {
            summary: RoutineSummary {
                slug,
                name: routine.name,
                description: routine.description,
                trigger: routine.trigger,
            },
            metadata: routine.metadata,
            steps: routine
                .steps
                .into_iter()
                .map(RoutineStepDocument::from)
                .collect(),
            edges: routine
                .edges
                .into_iter()
                .map(RoutineEdgeDocument::from)
                .collect(),
        }
    }
}

impl From<RoutineStepManifest> for RoutineStepDocument {
    fn from(step: RoutineStepManifest) -> Self {
        Self {
            slug: step.slug,
            routine: step.routine,
            name: step.name,
            step_type: step.step_type,
            council: step.council,
            agent: step.agent,
            config: step.config,
            order_index: step.order_index,
        }
    }
}

impl From<RoutineEdgeManifest> for RoutineEdgeDocument {
    fn from(edge: RoutineEdgeManifest) -> Self {
        Self {
            routine: edge.routine,
            source_step: edge.source_step,
            target_step: edge.target_step,
            condition: edge.condition,
            metadata: edge.metadata,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Model metadata returned by list/get operations.
pub struct ModelSummary {
    pub slug: Slug,
    pub name: String,
    pub description: Option<String>,
    pub model: String,
    pub model_provider: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Model document including runtime configuration.
pub struct ModelDocument {
    #[serde(flatten)]
    pub summary: ModelSummary,
    pub temperature: Option<f64>,
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub native_tools: Vec<NativeModelToolId>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
/// Request body for creating a model.
pub struct ModelCreateDocument {
    pub name: String,
    pub description: Option<String>,
    pub model: String,
    #[serde(default)]
    pub model_provider: Option<String>,
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub native_tools: Vec<NativeModelToolId>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
/// Partial update body for a model.
pub struct ModelUpdateDocument {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_tools: Option<Vec<NativeModelToolId>>,
}

impl From<ModelManifest> for ModelDocument {
    fn from(model: ModelManifest) -> Self {
        Self {
            summary: ModelSummary {
                slug: model.slug,
                name: model.name,
                description: model.description,
                model: model.model,
                model_provider: model.model_provider,
            },
            temperature: model.temperature,
            base_url: model.base_url,
            native_tools: model.native_tools,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Council metadata returned by list/get operations.
pub struct CouncilSummary {
    pub name: String,
    pub delegation_strategy: CouncilDelegationStrategy,
    pub leader_agent: Slug,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Council member entry embedded in a council document.
pub struct CouncilMemberDocument {
    pub agent: Slug,
    pub agent_name: String,
    pub priority: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Council document including membership state.
pub struct CouncilDocument {
    #[serde(flatten)]
    pub summary: CouncilSummary,
    #[serde(default)]
    pub members: Vec<CouncilMemberDocument>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Request body for creating one council member entry.
pub struct CouncilCreateMemberDocument {
    pub agent: Slug,
    #[serde(default)]
    pub priority: i32,
    #[serde(default)]
    pub config: serde_json::Value,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
/// Partial update body for one council member entry.
pub struct CouncilMemberUpdateDocument {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<serde_json::Value>,
}

impl CouncilMemberUpdateDocument {
    /// Return whether the update contains no effective field changes.
    pub fn is_empty(&self) -> bool {
        self.priority.is_none() && self.config.is_none()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Request body for creating a council.
pub struct CouncilCreateDocument {
    pub name: String,
    pub description: Option<String>,
    pub leader_agent: Slug,
    #[serde(default)]
    pub delegation_strategy: Option<CouncilDelegationStrategy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<serde_json::Value>,
    #[serde(default)]
    pub members: Vec<CouncilCreateMemberDocument>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
/// Partial update body for a council.
pub struct CouncilUpdateDocument {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegation_strategy: Option<CouncilDelegationStrategy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<serde_json::Value>,
}

impl From<CouncilManifest> for CouncilDocument {
    fn from(council: CouncilManifest) -> Self {
        Self {
            summary: CouncilSummary {
                name: council.name,
                delegation_strategy: council.delegation_strategy,
                leader_agent: council.leader_agent,
            },
            members: council
                .members
                .into_iter()
                .map(|member| CouncilMemberDocument {
                    agent: member.agent.clone(),
                    agent_name: member.agent.to_string(),
                    priority: member.priority,
                })
                .collect(),
        }
    }
}
