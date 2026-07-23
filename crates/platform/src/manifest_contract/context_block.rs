//! Context block wire records.

use chrono::{DateTime, Utc};
use nenjo::manifest::ContextBlockManifest;
use nenjo_events::EncryptedPayload;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::wire::PlatformRecord;

/// Metadata for a context block on REST, events, and worker sync.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextBlockRecord {
    pub id: Uuid,
    pub org_id: Uuid,
    pub slug: String,
    pub name: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default = "default_source_type")]
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

fn default_source_type() -> String {
    "native".to_string()
}

/// Content response for context block template routes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextBlockContentRecord {
    #[serde(flatten)]
    pub block: ContextBlockRecord,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encrypted_payload: Option<EncryptedPayload>,
}

impl ContextBlockRecord {
    #[allow(clippy::too_many_arguments)]
    pub fn from_parts(
        org_id: Uuid,
        id: Uuid,
        slug: String,
        name: String,
        path: String,
        description: Option<String>,
        source_type: String,
        read_only: bool,
        metadata: serde_json::Value,
        created_by: Option<Uuid>,
        created_at: DateTime<Utc>,
        updated_at: DateTime<Utc>,
    ) -> Self {
        Self {
            id,
            org_id,
            slug,
            name,
            path,
            description,
            source_type,
            read_only,
            metadata,
            created_by,
            created_at,
            updated_at,
        }
    }

    pub fn to_manifest(&self, template: String) -> ContextBlockManifest {
        ContextBlockManifest {
            slug: nenjo::Slug::parse(&self.slug)
                .unwrap_or_else(|_| nenjo::Slug::derive(&self.slug)),
            name: self.name.clone(),
            path: self.path.clone(),
            description: self.description.clone(),
            template,
        }
    }

    pub fn to_document(&self) -> crate::manifest_mcp::ContextBlockDocument {
        crate::manifest_mcp::ContextBlockDocument {
            summary: crate::manifest_mcp::ContextBlockSummary {
                name: self.name.clone(),
                slug: nenjo::Slug::derive(&self.slug),
                selector: format!("{{{{ {} }}}}", self.slug),
                path: self.path.clone(),
                description: self.description.clone(),
            },
            template: String::new(),
        }
    }
}

impl PlatformRecord for ContextBlockRecord {
    fn id(&self) -> Uuid {
        self.id
    }

    fn slug(&self) -> &str {
        &self.slug
    }
}

impl ContextBlockContentRecord {
    pub fn with_template(mut self, template: Option<String>) -> Self {
        self.template = template;
        self
    }

    pub fn to_manifest(&self) -> ContextBlockManifest {
        self.block
            .to_manifest(self.template.clone().unwrap_or_default())
    }

    pub fn to_document(&self) -> crate::manifest_mcp::ContextBlockDocument {
        crate::manifest_mcp::ContextBlockDocument::from(self.to_manifest())
    }
}
