//! Domain wire records.

use chrono::{DateTime, Utc};
use nenjo::Slug;
use nenjo::manifest::{DomainManifest, DomainPromptConfig, domain_slug};
use nenjo_events::EncryptedPayload;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::wire::PlatformRecord;

fn slug_from_str(value: &str) -> Slug {
    Slug::parse(value).unwrap_or_else(|_| Slug::derive(value))
}

fn default_domain_source_type() -> String {
    "native".to_string()
}

/// Metadata for a domain on REST, events, and worker sync.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DomainRecord {
    pub id: Uuid,
    pub org_id: Uuid,
    pub slug: String,
    pub name: String,
    #[serde(default)]
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub command: String,
    #[serde(default)]
    pub platform_scopes: Vec<String>,
    #[serde(default)]
    pub abilities: Vec<String>,
    #[serde(default)]
    pub mcp_servers: Vec<String>,
    #[serde(default)]
    pub script_tools: Vec<String>,
    #[serde(default = "default_domain_source_type")]
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

/// Domain metadata plus optional inline or encrypted prompt content.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainPromptRecord {
    #[serde(flatten)]
    pub domain: DomainRecord,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_config: Option<DomainPromptConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encrypted_payload: Option<EncryptedPayload>,
}

impl DomainRecord {
    pub fn slug_for_path_name(path: &str, name: &str) -> String {
        domain_slug(path, name).into_string()
    }

    pub fn to_manifest(&self, prompt_config: DomainPromptConfig) -> DomainManifest {
        DomainManifest {
            name: self.name.clone(),
            path: self.path.clone(),
            description: self.description.clone(),
            command: self.command.clone(),
            platform_scopes: self.platform_scopes.clone(),
            abilities: self.abilities.clone(),
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
            prompt_config,
        }
    }

    pub fn to_document(&self) -> crate::manifest_mcp::DomainDocument {
        let manifest = self.to_manifest(DomainPromptConfig::default());
        crate::manifest_mcp::DomainDocument::from(manifest)
    }
}

impl DomainPromptRecord {
    pub fn resolved_prompt_config(&self) -> DomainPromptConfig {
        self.prompt_config.clone().unwrap_or_default()
    }

    pub fn to_manifest(&self) -> DomainManifest {
        self.domain.to_manifest(self.resolved_prompt_config())
    }

    pub fn to_document(&self) -> crate::manifest_mcp::DomainPromptDocument {
        crate::manifest_mcp::DomainPromptDocument::from(self.to_manifest())
    }
}

impl PlatformRecord for DomainRecord {
    fn id(&self) -> Uuid {
        self.id
    }

    fn slug(&self) -> &str {
        &self.slug
    }
}
