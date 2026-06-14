use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Map;

macro_rules! resource_manifest_v1 {
    ($name:ident) => {
        #[derive(Debug, Clone, Serialize, Deserialize)]
        pub struct $name {
            pub name: String,
            #[serde(flatten)]
            pub fields: Map<String, serde_json::Value>,
        }
    };
}

resource_manifest_v1!(AgentManifestV1);
resource_manifest_v1!(AbilityManifestV1);
resource_manifest_v1!(DomainManifestV1);
resource_manifest_v1!(ContextBlockManifestV1);
resource_manifest_v1!(KnowledgeManifestV1);
resource_manifest_v1!(SkillManifestV1);
resource_manifest_v1!(PluginManifestV1);
resource_manifest_v1!(McpServerManifestV1);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutineManifestV1 {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub trigger: RoutineTriggerV1,
    #[serde(default)]
    pub metadata: Map<String, serde_json::Value>,
    #[serde(default)]
    pub entry_steps: Vec<String>,
    #[serde(default)]
    pub steps: Vec<RoutineStepManifestV1>,
    #[serde(default)]
    pub edges: Vec<RoutineEdgeManifestV1>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RoutineTriggerV1 {
    Kind(String),
    Config(RoutineTriggerConfigV1),
}

impl RoutineTriggerV1 {
    pub fn kind(&self) -> &str {
        match self {
            Self::Kind(kind) => kind,
            Self::Config(config) => config.kind.as_str(),
        }
    }

    pub fn metadata(&self) -> Map<String, serde_json::Value> {
        let mut metadata = Map::new();
        if let Self::Config(config) = self {
            if let Some(schedule) = &config.schedule {
                metadata.insert(
                    "schedule".to_string(),
                    serde_json::Value::String(schedule.clone()),
                );
            }
            if let Some(timezone) = &config.timezone {
                metadata.insert(
                    "timezone".to_string(),
                    serde_json::Value::String(timezone.clone()),
                );
            }
        }
        metadata
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutineTriggerConfigV1 {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub schedule: Option<String>,
    #[serde(default)]
    pub timezone: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutineStepManifestV1 {
    #[serde(rename = "ref")]
    pub step_ref: String,
    pub name: String,
    #[serde(rename = "type")]
    pub step_type: String,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub council: Option<String>,
    #[serde(default)]
    pub config: serde_json::Value,
    #[serde(default)]
    pub position: Option<RoutineStepPositionV1>,
    #[serde(default)]
    pub order_index: Option<i32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RoutineStepPositionV1 {
    #[serde(default)]
    pub x: f64,
    #[serde(default)]
    pub y: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutineEdgeManifestV1 {
    pub from: String,
    pub to: String,
    #[serde(default = "default_routine_edge_condition")]
    pub condition: String,
    #[serde(default)]
    pub max_attempts: Option<u32>,
    #[serde(default)]
    pub metadata: Map<String, serde_json::Value>,
}

fn default_routine_edge_condition() -> String {
    "always".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RepoResourceManifest {
    AgentV1(AgentManifestV1),
    AbilityV1(AbilityManifestV1),
    DomainV1(DomainManifestV1),
    ContextBlockV1(ContextBlockManifestV1),
    KnowledgeV1(KnowledgeManifestV1),
    SkillV1(SkillManifestV1),
    PluginV1(PluginManifestV1),
    McpServerV1(McpServerManifestV1),
    RoutineV1(RoutineManifestV1),
}

pub fn parse_resource_manifest(
    schema: &str,
    manifest: serde_json::Value,
) -> Result<RepoResourceManifest> {
    match schema {
        "nenjo.agent.v1" => Ok(RepoResourceManifest::AgentV1(parse_manifest_body(
            manifest,
        )?)),
        "nenjo.ability.v1" => Ok(RepoResourceManifest::AbilityV1(parse_manifest_body(
            manifest,
        )?)),
        "nenjo.domain.v1" => Ok(RepoResourceManifest::DomainV1(parse_manifest_body(
            manifest,
        )?)),
        "nenjo.context_block.v1" => Ok(RepoResourceManifest::ContextBlockV1(parse_manifest_body(
            manifest,
        )?)),
        "nenjo.knowledge.v1" => Ok(RepoResourceManifest::KnowledgeV1(parse_manifest_body(
            manifest,
        )?)),
        "nenjo.skill.v1" => Ok(RepoResourceManifest::SkillV1(parse_manifest_body(
            manifest,
        )?)),
        "nenjo.plugin.v1" => Ok(RepoResourceManifest::PluginV1(parse_manifest_body(
            manifest,
        )?)),
        "nenjo.mcp_server.v1" => Ok(RepoResourceManifest::McpServerV1(parse_manifest_body(
            manifest,
        )?)),
        "nenjo.routine.v1" => Ok(RepoResourceManifest::RoutineV1(parse_manifest_body(
            manifest,
        )?)),
        other => bail!("unsupported repo resource manifest schema '{other}'"),
    }
}

fn parse_manifest_body<T>(manifest: serde_json::Value) -> Result<T>
where
    T: for<'de> Deserialize<'de>,
{
    serde_json::from_value(manifest).context("failed to parse resource manifest body")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_routine_manifest_v1() {
        let value = serde_json::json!({
            "name": "review_resource_design",
            "description": "Review a proposed resource.",
            "trigger": {
                "type": "cron",
                "schedule": "0 9 * * 1-5",
                "timezone": "America/Chicago"
            },
            "entry_steps": ["review"],
            "steps": [
                {
                    "ref": "review",
                    "name": "Review",
                    "type": "agent",
                    "agent": "nenjo/agents/nenji/package.yaml",
                    "config": { "instructions": "Review the proposal." }
                },
                {
                    "ref": "failed",
                    "name": "Failed",
                    "type": "terminal_fail"
                }
            ],
            "edges": [
                {
                    "from": "review",
                    "to": "failed",
                    "condition": "on_fail",
                    "max_attempts": 3
                }
            ]
        });
        let parsed = parse_resource_manifest("nenjo.routine.v1", value).unwrap();
        let RepoResourceManifest::RoutineV1(routine) = parsed else {
            panic!("expected routine manifest");
        };
        assert_eq!(routine.name, "review_resource_design");
        assert_eq!(routine.trigger.kind(), "cron");
        assert_eq!(
            routine
                .trigger
                .metadata()
                .get("timezone")
                .and_then(|v| v.as_str()),
            Some("America/Chicago")
        );
        assert_eq!(routine.edges[0].max_attempts, Some(3));
    }
}
