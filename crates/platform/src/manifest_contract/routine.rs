//! Routine wire records.

use chrono::{DateTime, Utc};
use nenjo::Slug;
use nenjo::manifest::{
    RoutineEdgeCondition, RoutineEdgeManifest, RoutineManifest, RoutineMetadata,
    RoutineStepManifest, RoutineStepType, RoutineTrigger,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::wire::PlatformRecord;

fn slug_from_str(value: &str) -> Slug {
    Slug::parse(value).unwrap_or_else(|_| Slug::derive(value))
}

fn parse_trigger(value: &str) -> RoutineTrigger {
    match value.trim().to_ascii_lowercase().as_str() {
        "cron" => RoutineTrigger::Cron,
        _ => RoutineTrigger::Task,
    }
}

fn parse_step_type(value: &str) -> RoutineStepType {
    serde_json::from_value(serde_json::Value::String(value.to_string())).unwrap_or_default()
}

/// One routine step embedded in a routine wire record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutineStepRecord {
    pub id: Uuid,
    pub slug: String,
    pub routine: String,
    pub name: String,
    pub step_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub council: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default)]
    pub config: serde_json::Value,
    pub order_index: i32,
}

/// One routine edge embedded in a routine wire record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutineEdgeRecord {
    pub id: Uuid,
    pub routine: String,
    pub source_step: String,
    pub target_step: String,
    pub condition: String,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

/// Metadata for a routine on REST, events, and worker sync.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutineRecord {
    pub id: Uuid,
    pub org_id: Uuid,
    pub slug: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub trigger: String,
    #[serde(default)]
    pub metadata: serde_json::Value,
    #[serde(default)]
    pub steps: Vec<RoutineStepRecord>,
    #[serde(default)]
    pub edges: Vec<RoutineEdgeRecord>,
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
            trigger: parse_trigger(&self.trigger),
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
