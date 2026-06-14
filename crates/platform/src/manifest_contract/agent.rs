//! Agent wire records.

use chrono::{DateTime, Utc};
use nenjo::Slug;
use nenjo::manifest::PromptConfig;
use nenjo::manifest::{AgentHeartbeatManifest, AgentManifest};
use nenjo_events::EncryptedPayload;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::wire::PlatformRecord;

fn slug_from_str(value: &str) -> Slug {
    Slug::parse(value).unwrap_or_else(|_| Slug::derive(value))
}

fn default_agent_source_type() -> String {
    "native".to_string()
}

/// Metadata for an agent on REST, events, and worker sync.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRecord {
    pub id: Uuid,
    pub org_id: Uuid,
    pub slug: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_name: Option<String>,
    #[serde(default)]
    pub domains: Vec<String>,
    #[serde(default)]
    pub platform_scopes: Vec<String>,
    #[serde(default)]
    pub mcp_servers: Vec<String>,
    #[serde(default)]
    pub script_tools: Vec<String>,
    #[serde(default)]
    pub abilities: Vec<String>,
    #[serde(default)]
    pub prompt_locked: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heartbeat: Option<AgentHeartbeatManifest>,
    #[serde(default = "default_agent_source_type")]
    pub source_type: String,
    #[serde(default)]
    pub read_only: bool,
    #[serde(default)]
    pub metadata: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_config: Option<PromptConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encrypted_payload: Option<EncryptedPayload>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Agent metadata plus optional inline or encrypted prompt content.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentPromptRecord {
    #[serde(flatten)]
    pub agent: AgentRecord,
}

impl AgentRecord {
    pub fn resolved_prompt_config(&self) -> PromptConfig {
        self.prompt_config.clone().unwrap_or_default()
    }

    pub fn to_manifest(&self, prompt_config: PromptConfig) -> AgentManifest {
        AgentManifest {
            name: self.name.clone(),
            slug: Slug::derive(&self.slug),
            description: self.description.clone(),
            prompt_config,
            color: self.color.clone(),
            model: self.model.as_ref().map(|value| slug_from_str(value)),
            domains: self
                .domains
                .iter()
                .map(|value| slug_from_str(value))
                .collect(),
            platform_scopes: self.platform_scopes.clone(),
            mcp_servers: self
                .mcp_servers
                .iter()
                .map(|value| slug_from_str(value))
                .collect(),
            script_tools: self
                .script_tools
                .iter()
                .map(|value| slug_from_str(value))
                .collect(),
            abilities: self.abilities.clone(),
            prompt_locked: self.prompt_locked,
            heartbeat: self.heartbeat.clone(),
        }
    }

    pub fn to_document(&self) -> crate::manifest_mcp::AgentDocument {
        let manifest = self.to_manifest(self.resolved_prompt_config());
        crate::manifest_mcp::AgentDocument::from(manifest)
    }
}

impl AgentPromptRecord {
    pub fn resolved_prompt_config(&self) -> PromptConfig {
        self.agent.resolved_prompt_config()
    }

    pub fn to_manifest(&self) -> AgentManifest {
        self.agent.to_manifest(self.resolved_prompt_config())
    }

    pub fn to_document(&self) -> crate::manifest_mcp::AgentDocument {
        crate::manifest_mcp::AgentDocument::from(self.to_manifest())
    }
}

impl PlatformRecord for AgentRecord {
    fn id(&self) -> Uuid {
        self.id
    }

    fn slug(&self) -> &str {
        &self.slug
    }
}
