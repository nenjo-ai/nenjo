//! Canonical manifest MCP document types.

use derive_builder::Builder;
use nenjo::Slug;
use serde::{Deserialize, Deserializer, Serialize};

use nenjo::manifest::{
    AbilityManifest, AgentHeartbeatManifest, AgentManifest, ContextBlockManifest,
    CouncilDelegationStrategy, CouncilManifest, DomainManifest, ModelManifest, ProjectManifest,
    PromptConfig, RoutineEdgeCondition, RoutineEdgeManifest, RoutineManifest, RoutineMetadata,
    RoutineStepManifest, RoutineStepType, RoutineTrigger,
};
use nenjo::manifest::{AbilityPromptConfig, DomainPromptConfig, model_manifest_slug};

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

/// Canonical prompt-free agent document used by manifest get/update/delete operations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentDocument {
    #[serde(flatten)]
    pub summary: AgentSummary,
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

/// Full local agent state including prompt configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentPromptDocument {
    #[serde(flatten)]
    pub agent: AgentDocument,
    #[serde(default)]
    pub prompt_config: PromptConfig,
}

/// Agent fields that can be updated without using the prompt route.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentUpdateDocument {
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abilities: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domains: Option<Vec<Slug>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script_tools: Option<Vec<Slug>>,
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

impl From<AgentManifest> for AgentPromptDocument {
    fn from(agent: AgentManifest) -> Self {
        let prompt_config = agent.prompt_config.clone();
        Self {
            agent: AgentDocument::from(agent),
            prompt_config,
        }
    }
}

impl From<AgentDocument> for AgentUpdateDocument {
    fn from(agent: AgentDocument) -> Self {
        Self {
            name: Some(agent.summary.name),
            description: Some(agent.summary.description),
            color: Some(agent.summary.color),
            model: None,
            abilities: None,
            domains: None,
            script_tools: None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
/// Request body for creating an agent.
pub struct AgentCreateDocument {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<Slug>,
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
/// Ability document returned by metadata routes.
pub struct AbilityDocument {
    #[serde(flatten)]
    pub summary: AbilitySummary,
    pub activation_condition: String,
    #[serde(default)]
    pub platform_scopes: Vec<String>,
    #[serde(default)]
    pub mcp_servers: Vec<Slug>,
    #[serde(default)]
    pub script_tools: Vec<Slug>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Ability document including prompt configuration.
pub struct AbilityPromptDocument {
    #[serde(flatten)]
    pub ability: AbilityDocument,
    pub prompt_config: AbilityPromptConfig,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
/// Request body for creating an ability.
pub struct AbilityCreateDocument {
    pub name: String,
    #[serde(default)]
    pub path: String,
    pub description: Option<String>,
    #[serde(default)]
    pub activation_condition: String,
    pub prompt_config: AbilityPromptConfig,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_servers: Option<Vec<Slug>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script_tools: Option<Vec<Slug>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
/// Partial update body for an ability.
pub struct AbilityUpdateDocument {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activation_condition: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_servers: Option<Vec<Slug>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script_tools: Option<Vec<Slug>>,
}

impl AbilityUpdateDocument {
    /// Return whether the update contains no effective field changes.
    pub fn is_empty(&self) -> bool {
        self.name.is_none()
            && self.description.is_none()
            && self.activation_condition.is_none()
            && self.mcp_servers.is_none()
            && self.script_tools.is_none()
    }
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
            platform_scopes: ability.platform_scopes,
            mcp_servers: ability.mcp_servers,
            script_tools: ability.script_tools,
        }
    }
}

impl From<AbilityManifest> for AbilityPromptDocument {
    fn from(ability: AbilityManifest) -> Self {
        let prompt_config = ability.prompt_config.clone();
        Self {
            ability: AbilityDocument::from(ability),
            prompt_config,
        }
    }
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
/// Domain document returned by metadata routes.
pub struct DomainDocument {
    #[serde(flatten)]
    pub summary: DomainSummary,
    pub command: String,
    #[serde(default)]
    pub platform_scopes: Vec<String>,
    #[serde(default)]
    pub abilities: Vec<String>,
    #[serde(default)]
    pub mcp_servers: Vec<Slug>,
    #[serde(default)]
    pub script_tools: Vec<Slug>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Prompt-bearing domain document.
pub struct DomainPromptDocument {
    #[serde(flatten)]
    pub domain: DomainDocument,
    pub prompt_config: DomainPromptConfig,
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
/// Context block document returned by metadata routes.
pub struct ContextBlockDocument {
    #[serde(flatten)]
    pub summary: ContextBlockSummary,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Context block document including template content.
pub struct ContextBlockContentDocument {
    #[serde(flatten)]
    pub context_block: ContextBlockDocument,
    pub template: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
/// Request body for creating a context block.
pub struct ContextBlockCreateDocument {
    pub name: String,
    #[serde(default)]
    pub path: String,
    pub description: Option<String>,
    pub template: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
/// Partial update body for context block metadata or content.
pub struct ContextBlockUpdateDocument {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
/// Request body for creating a domain.
pub struct DomainCreateDocument {
    pub name: String,
    #[serde(default)]
    pub path: String,
    pub description: Option<String>,
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abilities: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_servers: Option<Vec<Slug>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script_tools: Option<Vec<Slug>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_config: Option<DomainPromptConfig>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
/// Partial update body for a domain document.
pub struct DomainUpdateDocument {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abilities: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_servers: Option<Vec<Slug>>,
    #[serde(default, skip_serializing)]
    pub script_tools: Option<Vec<Slug>>,
}

impl DomainUpdateDocument {
    /// Return whether the update contains no effective field changes.
    pub fn is_empty(&self) -> bool {
        self.name.is_none()
            && self.description.is_none()
            && self.command.is_none()
            && self.abilities.is_none()
            && self.mcp_servers.is_none()
            && self.script_tools.is_none()
    }
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
            platform_scopes: domain.platform_scopes,
            abilities: domain.abilities,
            mcp_servers: domain.mcp_servers,
            script_tools: domain.script_tools,
        }
    }
}

impl From<DomainManifest> for DomainPromptDocument {
    fn from(domain: DomainManifest) -> Self {
        let prompt_config = domain.prompt_config.clone();
        Self {
            domain: DomainDocument::from(domain),
            prompt_config,
        }
    }
}

/// Alias used by the current contract for a domain prompt document.
pub type DomainManifestDocument = DomainPromptDocument;

impl From<ContextBlockManifest> for ContextBlockDocument {
    fn from(context_block: ContextBlockManifest) -> Self {
        let slug = context_block.slug();
        let selector = format!(
            "{{{{ {} }}}}",
            context_block_selector(&context_block.path, &context_block.name)
        );
        Self {
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

impl From<ContextBlockManifest> for ContextBlockContentDocument {
    fn from(context_block: ContextBlockManifest) -> Self {
        let template = context_block.template.clone();
        Self {
            context_block: ContextBlockDocument::from(context_block),
            template,
        }
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
    pub status: String,
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
    #[builder(default)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
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
    pub target_doc: Slug,
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
/// Request body for creating a routine.
pub struct RoutineCreateDocument {
    pub name: String,
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger: Option<RoutineTrigger>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<RoutineMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph: Option<RoutineGraphInput>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
/// Partial update body for a routine.
pub struct RoutineUpdateDocument {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger: Option<RoutineTrigger>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<RoutineMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph: Option<RoutineGraphInput>,
}

impl RoutineUpdateDocument {
    /// Return whether the update contains no effective field changes.
    pub fn is_empty(&self) -> bool {
        self.name.is_none()
            && self.description.is_none()
            && self.trigger.is_none()
            && self.metadata.is_none()
            && self.graph.is_none()
    }
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
}

impl From<ModelManifest> for ModelDocument {
    fn from(model: ModelManifest) -> Self {
        let slug = model_manifest_slug(&model.model_provider, &model.model);
        Self {
            summary: ModelSummary {
                slug,
                name: model.name,
                description: model.description,
                model: model.model,
                model_provider: model.model_provider,
            },
            temperature: model.temperature,
            base_url: model.base_url,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn executable_resource_update_documents_do_not_serialize_platform_scopes() {
        let agent = serde_json::to_value(AgentUpdateDocument {
            name: Some("agent".into()),
            description: None,
            color: None,
            model: None,
            abilities: None,
            domains: None,
            script_tools: None,
        })
        .unwrap();
        assert!(agent.get("platform_scopes").is_none());

        let ability = serde_json::to_value(AbilityUpdateDocument {
            name: None,
            description: None,
            activation_condition: None,
            mcp_servers: None,
            script_tools: None,
        })
        .unwrap();
        assert!(ability.get("platform_scopes").is_none());

        let domain = serde_json::to_value(DomainUpdateDocument {
            name: None,
            description: None,
            command: None,
            abilities: None,
            mcp_servers: None,
            script_tools: None,
        })
        .unwrap();
        assert!(domain.get("platform_scopes").is_none());
    }
}
