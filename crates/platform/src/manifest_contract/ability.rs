//! Ability wire records.

use chrono::{DateTime, Utc};
use nenjo::Slug;
use nenjo::manifest::{AbilityManifest, AbilityPromptConfig};
use nenjo_events::EncryptedPayload;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::wire::PlatformRecord;

fn slug_from_str(value: &str) -> Slug {
    Slug::parse(value).unwrap_or_else(|_| Slug::derive(value))
}

fn default_ability_source_type() -> String {
    "native".to_string()
}

/// Metadata for an ability on REST, events, and worker sync.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AbilityRecord {
    pub id: Uuid,
    pub org_id: Uuid,
    pub slug: String,
    pub name: String,
    #[serde(default)]
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub activation_condition: String,
    #[serde(default)]
    pub platform_scopes: Vec<String>,
    #[serde(default)]
    pub mcp_servers: Vec<String>,
    #[serde(default)]
    pub script_tools: Vec<String>,
    #[serde(default = "default_ability_source_type")]
    pub source_type: String,
    #[serde(default)]
    pub read_only: bool,
    #[serde(default)]
    pub metadata: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Ability metadata plus optional inline or encrypted prompt content.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AbilityPromptRecord {
    #[serde(flatten)]
    pub ability: AbilityRecord,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_config: Option<AbilityPromptConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encrypted_payload: Option<EncryptedPayload>,
}

impl AbilityRecord {
    pub fn slug_for_name(name: &str) -> String {
        Slug::derive(name).into_string()
    }

    pub fn to_manifest(&self, prompt_config: AbilityPromptConfig) -> AbilityManifest {
        AbilityManifest {
            name: self.name.clone(),
            path: if self.path.is_empty() {
                None
            } else {
                Some(self.path.clone())
            },
            description: self.description.clone(),
            activation_condition: self.activation_condition.clone(),
            prompt_config,
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
            source_type: self.source_type.clone(),
            read_only: self.read_only,
            metadata: self.metadata.clone(),
        }
    }

    pub fn to_document(&self) -> crate::manifest_mcp::AbilityDocument {
        let manifest = self.to_manifest(AbilityPromptConfig::default());
        crate::manifest_mcp::AbilityDocument::from(manifest)
    }
}

impl AbilityPromptRecord {
    pub fn resolved_prompt_config(&self) -> AbilityPromptConfig {
        self.prompt_config.clone().unwrap_or_default()
    }

    pub fn to_manifest(&self) -> AbilityManifest {
        self.ability.to_manifest(self.resolved_prompt_config())
    }

    pub fn to_document(&self) -> crate::manifest_mcp::AbilityDocument {
        crate::manifest_mcp::AbilityDocument::from(self.to_manifest())
    }
}

impl PlatformRecord for AbilityRecord {
    fn id(&self) -> Uuid {
        self.id
    }

    fn slug(&self) -> &str {
        &self.slug
    }
}
