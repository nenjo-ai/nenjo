//! Council wire records.

use chrono::{DateTime, Utc};
use nenjo::Slug;
use nenjo::manifest::{CouncilDelegationStrategy, CouncilManifest, CouncilMemberManifest};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::wire::PlatformRecord;

fn slug_from_str(value: &str) -> Slug {
    Slug::parse(value).unwrap_or_else(|_| Slug::derive(value))
}

fn parse_delegation_strategy(value: &str) -> CouncilDelegationStrategy {
    serde_json::from_value(serde_json::Value::String(value.to_string())).unwrap_or_default()
}

/// One council member embedded in a council wire record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CouncilMemberRecord {
    pub agent: String,
    pub agent_name: String,
    pub priority: i32,
}

/// Metadata for a council on REST, events, and worker sync.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CouncilRecord {
    pub id: Uuid,
    pub org_id: Uuid,
    pub slug: String,
    pub name: String,
    pub delegation_strategy: String,
    pub leader_agent: String,
    #[serde(default)]
    pub members: Vec<CouncilMemberRecord>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl CouncilRecord {
    pub fn slug_for_name(name: &str) -> String {
        Slug::derive(name).into_string()
    }

    pub fn to_manifest(&self) -> CouncilManifest {
        CouncilManifest {
            name: self.name.clone(),
            delegation_strategy: parse_delegation_strategy(&self.delegation_strategy),
            leader_agent: slug_from_str(&self.leader_agent),
            members: self
                .members
                .iter()
                .map(|member| CouncilMemberManifest {
                    agent: slug_from_str(&member.agent),
                    priority: member.priority,
                })
                .collect(),
        }
    }

    pub fn to_document(&self) -> crate::manifest_mcp::CouncilDocument {
        crate::manifest_mcp::CouncilDocument::from(self.to_manifest())
    }
}

impl PlatformRecord for CouncilRecord {
    fn id(&self) -> Uuid {
        self.id
    }

    fn slug(&self) -> &str {
        &self.slug
    }
}
