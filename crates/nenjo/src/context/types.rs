//! Context types for prompt rendering.
//!
//! XML serialization is handled by quick-xml via `#[derive(Serialize)]`.

use std::collections::HashMap;

use serde::Serialize;
// ---------------------------------------------------------------------------
// Agent Specific Context
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename = "agent")]
pub struct AgentContext {
    #[serde(rename = "@slug")]
    pub slug: String,
    #[serde(rename = "@name")]
    pub display_name: String,
    #[serde(rename = "@llm_model_name")]
    pub model_name: String,
    #[serde(rename = "@description", skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

// ---------------------------------------------------------------------------
// Routines
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename = "routine")]
pub struct RoutineContext {
    #[serde(rename = "@slug")]
    pub slug: String,
    #[serde(rename = "@name")]
    pub name: String,
    #[serde(rename = "@execution_id")]
    pub execution_id: String,
    #[serde(rename = "@description", skip_serializing_if = "str_is_empty")]
    pub description: Option<String>,
    /// Current step context within the routine.
    #[serde(skip_serializing_if = "RoutineStepContext::is_empty")]
    pub step: RoutineStepContext,
    /// Structured handoffs routed to the current step.
    #[serde(skip_serializing_if = "RoutineHandoffsContext::is_empty")]
    pub handoffs: RoutineHandoffsContext,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename = "handoffs")]
pub struct RoutineHandoffsContext {
    #[serde(rename = "handoff", default)]
    pub items: Vec<RoutineHandoffContext>,
}

impl RoutineHandoffsContext {
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename = "handoff")]
pub struct RoutineHandoffContext {
    #[serde(rename = "@source_step")]
    pub source_step: String,
    #[serde(rename = "@target_step")]
    pub target_step: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub purpose: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    pub payload: String,
}

/// Context for the currently executing routine step.
#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename = "step")]
pub struct RoutineStepContext {
    #[serde(rename = "@name", skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(rename = "@type", skip_serializing_if = "String::is_empty")]
    pub step_type: String,
    /// Step instructions from the routine step config.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub instructions: String,
    /// Arbitrary metadata from the step config, serialized as a JSON string.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub metadata: String,
}

impl RoutineStepContext {
    pub fn is_empty(&self) -> bool {
        self.name.is_empty()
            && self.step_type.is_empty()
            && self.instructions.is_empty()
            && self.metadata.is_empty()
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
// Artifacts (document index injected into prompts)
// ---------------------------------------------------------------------------

/// A single artifact entry in the prompt index.
#[derive(Debug, Clone, Serialize)]
#[serde(rename = "artifact")]
pub struct ArtifactContext {
    #[serde(rename = "@name")]
    pub name: String,
    #[serde(rename = "@description")]
    pub description: String,
    #[serde(rename = "@created_by")]
    pub created_by: String,
    #[serde(rename = "@size")]
    pub size: String,
}

/// Project-scoped artifacts.
#[derive(Debug, Clone, Serialize)]
#[serde(rename = "project")]
pub struct ArtifactsProjectContext {
    #[serde(rename = "artifact")]
    pub artifacts: Vec<ArtifactContext>,
}

/// Workspace-global artifacts.
#[derive(Debug, Clone, Serialize)]
#[serde(rename = "workspace")]
pub struct ArtifactsWorkspaceContext {
    #[serde(rename = "artifact")]
    pub artifacts: Vec<ArtifactContext>,
}

/// All artifacts combined.
#[derive(Debug, Clone, Serialize)]
#[serde(rename = "artifacts")]
pub struct ArtifactsContext {
    #[serde(rename = "project", skip_serializing_if = "Option::is_none")]
    pub project: Option<ArtifactsProjectContext>,
    #[serde(rename = "workspace", skip_serializing_if = "Option::is_none")]
    pub workspace: Option<ArtifactsWorkspaceContext>,
}

// ---------------------------------------------------------------------------
// Task (current/active) cron
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
    #[serde(rename = "@name")]
    pub name: String,
    #[serde(rename = "@slug", skip_serializing_if = "String::is_empty")]
    pub slug: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub working_dir: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub context: String,
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
        self.slug.is_empty() || self.name.is_empty()
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
            name: project.name.clone(),
            slug: project.slug.to_string(),
            description: project.description.clone().unwrap_or_default(),
            context: project
                .settings
                .get("context")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
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
    /// Owning package name when known (e.g. `@nenjo-ai/context`).
    pub package_name: Option<String>,
    /// Package version when known (e.g. `1.0.4`).
    pub package_version: Option<String>,
}

impl RenderContextBlock {
    pub fn new(
        name: impl Into<String>,
        path: impl Into<String>,
        template: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            path: path.into(),
            template: template.into(),
            package_name: None,
            package_version: None,
        }
    }
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
            slug: "coder".into(),
            display_name: "Cody".into(),
            model_name: "gpt-4".into(),
            description: Some("Writes code".into()),
        };
        let xml = nenjo_xml::to_xml(&agent);
        assert!(xml.contains("slug=\"coder\""));
        assert!(xml.contains("name=\"Cody\""));
        assert!(xml.contains("description=\"Writes code\""));
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
    fn test_project_context_xml() {
        let project = ProjectContext {
            name: "MyApp".into(),
            slug: "myapp".into(),
            description: "A cool app".into(),
            working_dir: "/home/user/myapp".into(),
            context: "Use postgres".into(),
            metadata: String::new(),
            git: Some(GitContext {
                repo_url: String::new(),
                branch: "main".into(),
                target_branch: String::new(),
                work_dir: String::new(),
            }),
        };
        let xml = nenjo_xml::to_xml_pretty(&project, 2);
        assert!(xml.contains("slug=\"myapp\""));
        assert!(xml.contains("name=\"MyApp\""));
        assert!(xml.contains("<description>A cool app</description>"));
        assert!(xml.contains("<context>Use postgres</context>"));
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
