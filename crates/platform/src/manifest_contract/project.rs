//! Project wire records.

use chrono::{DateTime, Utc};
use nenjo::Slug;
use nenjo::manifest::ProjectManifest;
use nenjo_events::EncryptedPayload;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::wire::PlatformRecord;

/// Metadata for a project on REST, events, and worker sync.
///
/// Settings ciphertext is carried separately via [`SensitiveContentKind::ProjectSettings`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectRecord {
    pub id: Uuid,
    pub org_id: Uuid,
    pub slug: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl ProjectRecord {
    pub fn to_manifest(&self, settings: serde_json::Value) -> ProjectManifest {
        ProjectManifest {
            name: self.name.clone(),
            slug: Slug::derive(&self.slug),
            description: self.description.clone(),
            settings,
        }
    }

    pub fn to_document(&self) -> crate::manifest_mcp::ProjectDocument {
        crate::manifest_mcp::ProjectDocument {
            summary: crate::manifest_mcp::ProjectSummary {
                name: self.name.clone(),
                slug: Slug::derive(&self.slug),
                description: self.description.clone(),
            },
            settings: serde_json::Value::Object(Default::default()),
        }
    }
}

impl PlatformRecord for ProjectRecord {
    fn id(&self) -> Uuid {
        self.id
    }

    fn slug(&self) -> &str {
        &self.slug
    }
}

/// REST project detail including org-shared settings.
///
/// Settings ciphertext belongs in `encrypted_payload` (OCK domain); plaintext settings
/// remain in `settings` until encrypted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectDetailRecord {
    #[serde(flatten)]
    pub project: ProjectRecord,
    pub settings: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encrypted_payload: Option<EncryptedPayload>,
}
