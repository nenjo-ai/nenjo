//! Model wire records.

use chrono::{DateTime, Utc};
use nenjo::Slug;
use nenjo::manifest::{ModelManifest, model_manifest_slug};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::wire::PlatformRecord;

/// Metadata for a model on REST, events, and worker sync.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelRecord {
    pub id: Uuid,
    pub org_id: Uuid,
    pub slug: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub model: String,
    pub model_provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    pub created_by: Uuid,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl ModelRecord {
    pub fn slug_for_provider_model(model_provider: &str, model: &str) -> String {
        model_manifest_slug(model_provider, model).into_string()
    }

    pub fn to_manifest(&self) -> ModelManifest {
        ModelManifest {
            name: self.name.clone(),
            slug: Slug::derive(&self.slug),
            description: self.description.clone(),
            model: self.model.clone(),
            model_provider: self.model_provider.clone(),
            temperature: self.temperature,
            base_url: self.base_url.clone(),
        }
    }

    pub fn to_document(&self) -> crate::manifest_mcp::ModelDocument {
        let manifest = self.to_manifest();
        crate::manifest_mcp::ModelDocument::from(manifest)
    }
}

impl PlatformRecord for ModelRecord {
    fn id(&self) -> Uuid {
        self.id
    }

    fn slug(&self) -> &str {
        &self.slug
    }
}
