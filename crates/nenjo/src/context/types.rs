//! Context types for prompt rendering.
//!
//! Each entity has a singular context struct (e.g. `AgentContext`) and a plural
//! "available" wrapper (e.g. `AvailableAgentsContext`). XML serialization is
//! handled by quick-xml via `#[derive(Serialize)]`.
//!
//! Singular types represent the current/active entity. Plural types represent
//! all available entities of that kind. Both serialize to XML via serde.

use std::collections::HashMap;

use serde::Serialize;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Agent Specific Context
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename = "available_agents")]
pub struct AvailableAgentsContext {
    #[serde(rename = "agent")]
    pub agents: Vec<AgentContext>,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename = "agent")]
pub struct AgentContext {
    #[serde(rename = "@id")]
    pub id: Uuid,
    #[serde(rename = "@role")]
    pub role: String,
    #[serde(rename = "@name")]
    pub display_name: String,
    #[serde(rename = "@llm_model_name")]
    pub model_name: String,
    #[serde(rename = "@description", skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename = "available_abilities")]
pub struct AvailableAbilitiesContext {
    #[serde(rename = "ability")]
    pub abilities: Vec<AbilityContext>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename = "ability")]
pub struct AbilityContext {
    #[serde(rename = "@name")]
    pub name: String,
    #[serde(rename = "@tool")]
    pub tool_name: String,
    #[serde(rename = "@use_when")]
    pub activate_when: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename = "available_domains")]
pub struct AvailableDomainsContext {
    #[serde(rename = "domain")]
    pub domains: Vec<DomainContext>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename = "domain")]
pub struct DomainContext {
    #[serde(rename = "@name")]
    pub name: String,
    #[serde(skip)]
    pub display_name: String,
    #[serde(rename = "@command")]
    pub command: String,
    #[serde(rename = "@description", skip_serializing_if = "str_is_empty")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename = "available_mcp_servers")]
pub struct AvailableMcpServersContext {
    #[serde(rename = "server")]
    pub servers: Vec<McpServerContext>,
    pub platform: Option<PlatformScopesContext>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename = "server")]
pub struct McpServerContext {
    #[serde(rename = "@name")]
    pub name: String,
    #[serde(rename = "@description", skip_serializing_if = "String::is_empty")]
    pub description: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename = "platform")]
pub struct PlatformScopesContext {
    #[serde(rename = "@scopes")]
    pub scopes: String,
}

// ---------------------------------------------------------------------------
// Routines
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename = "available_routines")]
pub struct AvailableRoutinesContext {
    #[serde(rename = "routine")]
    pub routines: Vec<RoutineContext>,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename = "routine")]
pub struct RoutineContext {
    #[serde(rename = "@name")]
    pub name: String,
    #[serde(rename = "@id")]
    pub id: Uuid,
    #[serde(rename = "@execution_id")]
    pub execution_id: String,
    #[serde(rename = "@description", skip_serializing_if = "str_is_empty")]
    pub description: Option<String>,
    /// Current step context within the routine.
    #[serde(skip_serializing_if = "RoutineStepContext::is_empty")]
    pub step: RoutineStepContext,
}

/// Context for the currently executing routine step.
#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename = "step")]
pub struct RoutineStepContext {
    #[serde(rename = "@name", skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(rename = "@type", skip_serializing_if = "String::is_empty")]
    pub step_type: String,
    /// Arbitrary metadata from the step config, serialized as a JSON string.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub metadata: String,
}

impl RoutineStepContext {
    pub fn is_empty(&self) -> bool {
        self.name.is_empty() && self.step_type.is_empty() && self.metadata.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename = "memory_profile")]
pub struct MemoryProfileContext {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub core_focus: Option<FocusListContext>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_focus: Option<FocusListContext>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shared_focus: Option<FocusListContext>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FocusListContext {
    #[serde(rename = "item")]
    pub items: Vec<String>,
}

// ---------------------------------------------------------------------------
// Memories (category facts injected into prompts)
// ---------------------------------------------------------------------------

/// A single memory category with its facts as text content.
#[derive(Debug, Clone, Serialize)]
#[serde(rename = "category")]
pub struct MemoryCategoryContext {
    #[serde(rename = "@name")]
    pub name: String,
    #[serde(rename = "$text")]
    pub text: String,
}

/// Core tier — agent's cross-project memories.
#[derive(Debug, Clone, Serialize)]
#[serde(rename = "memories-core")]
pub struct MemoriesCoreContext {
    #[serde(rename = "category")]
    pub categories: Vec<MemoryCategoryContext>,
}

/// Project tier — agent's memories for the current project.
#[derive(Debug, Clone, Serialize)]
#[serde(rename = "memories-project")]
pub struct MemoriesProjectContext {
    #[serde(rename = "category")]
    pub categories: Vec<MemoryCategoryContext>,
}

/// Shared tier — project memories shared across agents.
#[derive(Debug, Clone, Serialize)]
#[serde(rename = "memories-shared")]
pub struct MemoriesSharedContext {
    #[serde(rename = "category")]
    pub categories: Vec<MemoryCategoryContext>,
}

/// All memory tiers combined.
#[derive(Debug, Clone, Serialize)]
#[serde(rename = "memories")]
pub struct MemoriesContext {
    #[serde(rename = "memories-core", skip_serializing_if = "Option::is_none")]
    pub core: Option<MemoriesCoreContext>,
    #[serde(rename = "memories-project", skip_serializing_if = "Option::is_none")]
    pub project: Option<MemoriesProjectContext>,
    #[serde(rename = "memories-shared", skip_serializing_if = "Option::is_none")]
    pub shared: Option<MemoriesSharedContext>,
}

// ---------------------------------------------------------------------------
// Resources (document index injected into prompts)
// ---------------------------------------------------------------------------

/// A single resource entry in the prompt index.
#[derive(Debug, Clone, Serialize)]
#[serde(rename = "resource")]
pub struct ResourceContext {
    #[serde(rename = "@name")]
    pub name: String,
    #[serde(rename = "@description")]
    pub description: String,
    #[serde(rename = "@created_by")]
    pub created_by: String,
    #[serde(rename = "@size")]
    pub size: String,
}

/// Project-scoped resources.
#[derive(Debug, Clone, Serialize)]
#[serde(rename = "project")]
pub struct ResourcesProjectContext {
    #[serde(rename = "resource")]
    pub resources: Vec<ResourceContext>,
}

/// Workspace-global resources.
#[derive(Debug, Clone, Serialize)]
#[serde(rename = "workspace")]
pub struct ResourcesWorkspaceContext {
    #[serde(rename = "resource")]
    pub resources: Vec<ResourceContext>,
}

/// All resources combined.
#[derive(Debug, Clone, Serialize)]
#[serde(rename = "resources")]
pub struct ResourcesContext {
    #[serde(rename = "project", skip_serializing_if = "Option::is_none")]
    pub project: Option<ResourcesProjectContext>,
    #[serde(rename = "workspace", skip_serializing_if = "Option::is_none")]
    pub workspace: Option<ResourcesWorkspaceContext>,
}

// ---------------------------------------------------------------------------
// Documents (project document listing)
// ---------------------------------------------------------------------------

/// A single document entry in the project listing.
#[derive(Debug, Clone, Serialize)]
#[serde(rename = "doc")]
pub struct DocumentContext {
    #[serde(rename = "@name")]
    pub name: String,
    #[serde(rename = "@size")]
    pub size: String,
}

/// Project documents listing.
#[derive(Debug, Clone, Serialize)]
#[serde(rename = "project_documents")]
pub struct ProjectDocumentsContext {
    #[serde(rename = "@path")]
    pub path: String,
    #[serde(rename = "doc")]
    pub documents: Vec<DocumentContext>,
}

// ---------------------------------------------------------------------------
// Task (current/active) cron, gate
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename = "task")]
pub struct TaskContext {
    #[serde(rename = "@id")]
    pub id: String,
    #[serde(rename = "@slug")]
    pub slug: String,
    #[serde(rename = "@status")]
    pub status: String,
    #[serde(rename = "@priority")]
    pub priority: String,
    #[serde(rename = "@type")]
    pub task_type: String,
    pub title: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub acceptance_criteria: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub tags: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub source: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub complexity: String,
}

impl TaskContext {
    pub fn from_vars(vars: &HashMap<String, String>) -> Self {
        Self {
            id: vars.get("task.id").cloned().unwrap_or_default(),
            slug: vars.get("task.slug").cloned().unwrap_or_default(),
            title: vars.get("task.title").cloned().unwrap_or_default(),
            description: vars.get("task.description").cloned().unwrap_or_default(),
            acceptance_criteria: vars
                .get("task.acceptance_criteria")
                .cloned()
                .unwrap_or_default(),
            tags: vars.get("task.tags").cloned().unwrap_or_default(),
            source: vars.get("task.source").cloned().unwrap_or_default(),
            status: vars.get("task.status").cloned().unwrap_or_default(),
            priority: vars.get("task.priority").cloned().unwrap_or_default(),
            task_type: vars.get("task.type").cloned().unwrap_or_default(),
            complexity: vars.get("task.complexity").cloned().unwrap_or_default(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.id.is_empty()
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename = "gate_evaluation")]
pub struct GateContext {
    #[serde(skip_serializing_if = "String::is_empty")]
    pub criteria: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub previous_output: String,
}

impl GateContext {
    pub fn from_vars(vars: &HashMap<String, String>) -> Self {
        Self {
            criteria: vars.get("gate.criteria").cloned().unwrap_or_default(),
            previous_output: vars
                .get("gate.previous_output")
                .cloned()
                .unwrap_or_default(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.criteria.is_empty() && self.previous_output.is_empty()
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename = "cron_execution")]
pub struct CronContext {
    pub scheduled_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<TaskContext>,
}

impl CronContext {
    /// Create a new CronContext from a HashMap of variables.
    pub fn from_vars(vars: &HashMap<String, String>) -> Self {
        let task = TaskContext::from_vars(vars);
        Self {
            scheduled_at: vars.get("global.timestamp").cloned().unwrap_or_default(),
            task: if task.is_empty() { None } else { Some(task) },
        }
    }
}

// ---------------------------------------------------------------------------
// Project (current/active)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename = "git")]
pub struct GitContext {
    #[serde(rename = "@repo_url", skip_serializing_if = "String::is_empty")]
    pub repo_url: String,
    #[serde(rename = "@current_branch", skip_serializing_if = "String::is_empty")]
    pub branch: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub target_branch: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub work_dir: String,
}

impl GitContext {
    pub fn is_empty(&self) -> bool {
        self.repo_url.is_empty()
            && self.branch.is_empty()
            && self.target_branch.is_empty()
            && self.work_dir.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename = "project")]
pub struct ProjectContext {
    #[serde(rename = "@id")]
    pub id: String,
    #[serde(rename = "@name")]
    pub name: String,
    #[serde(rename = "@slug", skip_serializing_if = "String::is_empty")]
    pub slug: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub working_dir: String,
    /// Custom key-value metadata from project settings, serialized as XML.
    /// Skipped from XML serialization because it contains raw XML that would
    /// be double-escaped. Accessed via `{{ project.metadata }}` as a flat var.
    #[serde(skip)]
    pub metadata: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git: Option<GitContext>,
}

impl ProjectContext {
    pub fn is_empty(&self) -> bool {
        self.id.is_empty() || self.name.is_empty() || self.id == Uuid::nil().to_string()
    }

    /// Build from a manifest entry, resolving git context from project settings.
    pub fn from_manifest(project: &crate::manifest::ProjectManifest) -> Self {
        let git = project
            .settings
            .get("repo_sync_status")
            .and_then(|v| v.as_str())
            .filter(|s| *s == "synced")
            .map(|_| {
                let repo_url = project
                    .settings
                    .get("repo_url")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                GitContext {
                    repo_url,
                    ..Default::default()
                }
            });

        Self {
            id: project.id.to_string(),
            name: project.name.clone(),
            slug: project.slug.clone(),
            description: project.description.clone().unwrap_or_default(),
            metadata: nenjo_xml::types::metadata_json_to_xml(&project.settings),
            working_dir: String::new(),
            git,
        }
    }
}

// ---------------------------------------------------------------------------
// Context block template
// ---------------------------------------------------------------------------

/// Context block template (path + name → template text).
#[derive(Debug, Clone)]
pub struct RenderContextBlock {
    pub name: String,
    pub path: String,
    pub template: String,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn str_is_empty(s: &Option<String>) -> bool {
    s.as_ref().is_none_or(|s| s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_context_xml() {
        let agent = AgentContext {
            id: uuid::Uuid::nil(),
            role: "coder".into(),
            display_name: "Cody".into(),
            model_name: "gpt-4".into(),
            description: Some("Writes code".into()),
        };
        let xml = nenjo_xml::to_xml(&agent);
        assert!(xml.contains("role=\"coder\""));
        assert!(xml.contains("name=\"Cody\""));
        assert!(xml.contains("description=\"Writes code\""));
    }

    #[test]
    fn test_available_agents_xml() {
        let agents = AvailableAgentsContext {
            agents: vec![
                AgentContext {
                    id: uuid::Uuid::nil(),
                    role: "coder".into(),
                    display_name: "Cody".into(),
                    model_name: "gpt-4".into(),
                    description: Some("Writes code".into()),
                },
                AgentContext {
                    id: uuid::Uuid::nil(),
                    role: "reviewer".into(),
                    display_name: "Rex".into(),
                    model_name: "claude-4".into(),
                    description: None,
                },
            ],
        };
        let xml = nenjo_xml::to_xml_pretty(&agents, 2);
        assert!(xml.contains("<available_agents>"));
        assert!(xml.contains("role=\"coder\""));
        assert!(xml.contains("role=\"reviewer\""));
        assert!(xml.contains("</available_agents>"));
    }

    #[test]
    fn test_ability_context_xml() {
        let abilities = AvailableAbilitiesContext {
            abilities: vec![AbilityContext {
                name: "search".into(),
                tool_name: "search".into(),
                activate_when: "user asks to find something".into(),
            }],
        };
        let xml = nenjo_xml::to_xml_pretty(&abilities, 2);
        assert!(xml.contains("<available_abilities>"));
        assert!(xml.contains("name=\"search\""));
        assert!(xml.contains("tool=\"search\""));
        assert!(xml.contains("use_when=\"user asks to find something\""));
    }

    #[test]
    fn test_domain_context_xml() {
        let domains = AvailableDomainsContext {
            domains: vec![DomainContext {
                name: "prd".into(),
                display_name: "PRD Mode".into(),
                command: "/prd".into(),
                description: Some("Product requirements".into()),
            }],
        };
        let xml = nenjo_xml::to_xml_pretty(&domains, 2);
        assert!(xml.contains("<available_domains>"));
        assert!(xml.contains("command=\"/prd\""));
        assert!(xml.contains("description=\"Product requirements\""));
        // display_name is skipped
        assert!(!xml.contains("display_name"));
    }

    #[test]
    fn test_routine_context_xml() {
        let routines = AvailableRoutinesContext {
            routines: vec![RoutineContext {
                name: "deploy".into(),
                id: uuid::Uuid::nil(),
                execution_id: String::new(),
                description: Some("Deploy to prod".into()),
                step: Default::default(),
            }],
        };
        let xml = nenjo_xml::to_xml_pretty(&routines, 2);
        assert!(xml.contains("<available_routines>"));
        assert!(xml.contains("name=\"deploy\""));
    }

    #[test]
    fn test_mcp_servers_xml() {
        let mcp = AvailableMcpServersContext {
            servers: vec![McpServerContext {
                name: "github".into(),
                description: "GitHub integration".into(),
            }],
            platform: Some(PlatformScopesContext {
                scopes: "tickets:read, projects:write".into(),
            }),
        };
        let xml = nenjo_xml::to_xml_pretty(&mcp, 2);
        assert!(xml.contains("<available_mcp_servers>"));
        assert!(xml.contains("name=\"github\""));
        assert!(xml.contains("scopes=\"tickets:read, projects:write\""));
    }

    #[test]
    fn test_memory_profile_xml() {
        let profile = MemoryProfileContext {
            core_focus: Some(FocusListContext {
                items: vec!["architecture".into(), "patterns".into()],
            }),
            project_focus: None,
            shared_focus: None,
        };
        let xml = nenjo_xml::to_xml_pretty(&profile, 2);
        assert!(xml.contains("<memory_profile>"));
        assert!(xml.contains("<core_focus>"));
        assert!(xml.contains("<item>architecture</item>"));
        assert!(!xml.contains("project_focus"));
    }

    #[test]
    fn test_task_context_xml() {
        let task = TaskContext {
            id: "TASK-42".into(),
            slug: "fix-bug".into(),
            status: "open".into(),
            priority: "high".into(),
            task_type: "task".into(),
            title: "Fix login bug".into(),
            description: "SSO is broken".into(),
            acceptance_criteria: String::new(),
            tags: String::new(),
            source: String::new(),
            complexity: String::new(),
        };
        let xml = nenjo_xml::to_xml_pretty(&task, 2);
        assert!(xml.contains("id=\"TASK-42\""));
        assert!(xml.contains("<title>Fix login bug</title>"));
        assert!(xml.contains("<description>SSO is broken</description>"));
        // Empty fields should be omitted
        assert!(!xml.contains("acceptance_criteria"));
    }

    #[test]
    fn test_gate_context_xml() {
        let gate = GateContext {
            criteria: "All tests pass".into(),
            previous_output: "3 tests failed".into(),
        };
        let xml = nenjo_xml::to_xml_pretty(&gate, 2);
        assert!(xml.contains("<gate_evaluation>"));
        assert!(xml.contains("<criteria>All tests pass</criteria>"));
    }

    #[test]
    fn test_cron_context_xml() {
        let cron = CronContext {
            scheduled_at: "2026-03-28T10:00:00Z".into(),
            task: Some(TaskContext {
                id: "TASK-1".into(),
                slug: "daily".into(),
                status: "open".into(),
                priority: "low".into(),
                task_type: "cron".into(),
                title: "Daily check".into(),
                description: String::new(),
                acceptance_criteria: String::new(),
                tags: String::new(),
                source: String::new(),
                complexity: String::new(),
            }),
        };
        let xml = nenjo_xml::to_xml_pretty(&cron, 2);
        assert!(xml.contains("<cron_execution>"));
        assert!(xml.contains("<scheduled_at>2026-03-28T10:00:00Z</scheduled_at>"));
        assert!(xml.contains("<task"));
        assert!(xml.contains("id=\"TASK-1\""));
    }

    #[test]
    fn test_project_context_xml() {
        let project = ProjectContext {
            id: "proj-1".into(),
            name: "MyApp".into(),
            slug: "myapp".into(),
            description: "A cool app".into(),
            working_dir: "/home/user/myapp".into(),
            metadata: String::new(),
            git: Some(GitContext {
                repo_url: String::new(),
                branch: "main".into(),
                target_branch: String::new(),
                work_dir: String::new(),
            }),
        };
        let xml = nenjo_xml::to_xml_pretty(&project, 2);
        assert!(xml.contains("id=\"proj-1\""));
        assert!(xml.contains("name=\"MyApp\""));
        assert!(xml.contains("<description>A cool app</description>"));
        assert!(xml.contains("<git"));
        assert!(xml.contains("current_branch=\"main\""));
    }

    #[test]
    fn test_task_from_vars() {
        let mut vars = HashMap::new();
        vars.insert("task.id".into(), "T-1".into());
        vars.insert("task.title".into(), "Test".into());
        vars.insert("task.status".into(), "open".into());

        let task = TaskContext::from_vars(&vars);
        assert_eq!(task.id, "T-1");
        assert_eq!(task.title, "Test");
        assert!(!task.is_empty());
    }

    #[test]
    fn test_empty_task_from_vars() {
        let task = TaskContext::from_vars(&HashMap::new());
        assert!(task.is_empty());
    }
}
