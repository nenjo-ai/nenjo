//! Routine wire records.

use chrono::{DateTime, Utc};
use nenjo::Slug;
use nenjo::manifest::{
    RoutineEdgeCondition, RoutineEdgeManifest, RoutineManifest, RoutineMetadata,
    RoutineStepManifest, RoutineStepType,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::wire::PlatformRecord;

fn slug_from_str(value: &str) -> Slug {
    Slug::parse(value).unwrap_or_else(|_| Slug::derive(value))
}

fn parse_step_type(value: &str) -> RoutineStepType {
    serde_json::from_value(serde_json::Value::String(value.to_string())).unwrap_or_default()
}

/// One routine step embedded in a routine wire record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoutineStepRecord {
    pub id: Uuid,
    pub routine_id: Uuid,
    pub slug: String,
    pub routine: String,
    pub name: String,
    pub step_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub council_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub council: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lambda_id: Option<Uuid>,
    #[serde(default)]
    pub config: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encrypted_payload: Option<serde_json::Value>,
    pub position_x: f64,
    pub position_y: f64,
    pub order_index: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One routine edge embedded in a routine wire record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoutineEdgeRecord {
    pub id: Uuid,
    pub routine_id: Uuid,
    pub routine: String,
    pub source_step_id: Uuid,
    pub source_step: String,
    pub target_step_id: Uuid,
    pub target_step: String,
    pub condition: String,
    #[serde(default)]
    pub metadata: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

/// Metadata for a routine on REST, events, and worker sync.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoutineRecord {
    pub id: Uuid,
    pub org_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<Uuid>,
    pub slug: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub is_active: bool,
    pub is_default: bool,
    pub max_retries: i32,
    pub step_count: i64,
    #[serde(default)]
    pub metadata: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encrypted_payload: Option<serde_json::Value>,
    #[serde(default)]
    pub steps: Vec<RoutineStepRecord>,
    #[serde(default)]
    pub edges: Vec<RoutineEdgeRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_run_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl RoutineStepRecord {
    pub fn to_manifest(&self) -> RoutineStepManifest {
        RoutineStepManifest {
            slug: slug_from_str(&self.slug),
            routine: slug_from_str(&self.routine),
            name: self.name.clone(),
            step_type: parse_step_type(&self.step_type),
            council: self.council.as_ref().map(|value| slug_from_str(value)),
            agent: self.agent.as_ref().map(|value| slug_from_str(value)),
            config: self.config.clone(),
            order_index: self.order_index,
        }
    }
}

impl RoutineEdgeRecord {
    pub fn to_manifest(&self) -> RoutineEdgeManifest {
        RoutineEdgeManifest {
            routine: slug_from_str(&self.routine),
            source_step: slug_from_str(&self.source_step),
            target_step: slug_from_str(&self.target_step),
            condition: RoutineEdgeCondition::from_str_value(&self.condition),
            metadata: self.metadata.clone(),
        }
    }
}

impl RoutineRecord {
    fn routine_metadata(&self) -> RoutineMetadata {
        serde_json::from_value(self.metadata.clone()).unwrap_or_default()
    }

    pub fn to_manifest(&self) -> RoutineManifest {
        RoutineManifest {
            name: self.name.clone(),
            slug: slug_from_str(&self.slug),
            description: self.description.clone(),
            metadata: self.routine_metadata(),
            steps: self
                .steps
                .iter()
                .map(RoutineStepRecord::to_manifest)
                .collect(),
            edges: self
                .edges
                .iter()
                .map(RoutineEdgeRecord::to_manifest)
                .collect(),
        }
    }

    pub fn to_document(&self) -> crate::manifest_mcp::RoutineDocument {
        crate::manifest_mcp::RoutineDocument::from(self.to_manifest())
    }
}

impl PlatformRecord for RoutineRecord {
    fn id(&self) -> Uuid {
        self.id
    }

    fn slug(&self) -> &str {
        &self.slug
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routine_record_to_manifest_preserves_edge_handoff_metadata() {
        let now = Utc::now();
        let routine_id = Uuid::new_v4();
        let record = RoutineRecord {
            id: routine_id,
            org_id: Uuid::new_v4(),
            project_id: None,
            slug: "handoff-routine".to_string(),
            name: "Handoff Routine".to_string(),
            description: None,
            is_active: true,
            is_default: false,
            max_retries: 3,
            step_count: 0,
            metadata: serde_json::json!({ "entry_steps": ["source"] }),
            encrypted_payload: None,
            steps: Vec::new(),
            edges: vec![RoutineEdgeRecord {
                id: Uuid::new_v4(),
                routine_id,
                routine: "handoff-routine".to_string(),
                source_step_id: Uuid::new_v4(),
                source_step: "source".to_string(),
                target_step_id: Uuid::new_v4(),
                target_step: "target".to_string(),
                condition: "always".to_string(),
                metadata: serde_json::json!({
                    "purpose": "batch_1",
                    "handoff_instructions": "Send the first batch only"
                }),
                created_at: now,
            }],
            last_run_at: None,
            next_run_at: None,
            created_by: None,
            created_at: now,
            updated_at: now,
        };

        let manifest = record.to_manifest();

        assert_eq!(manifest.edges[0].metadata["purpose"], "batch_1");
        assert_eq!(
            manifest.edges[0].metadata["handoff_instructions"],
            "Send the first batch only"
        );
    }
}
