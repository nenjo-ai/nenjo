//! Memory types: categories, facts, and scopes.

use serde::{Deserialize, Serialize};

/// A single fact within a category.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryFact {
    pub text: String,
    pub created_at: String,
}

/// A category of memories (one file on disk).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryCategory {
    pub category: String,
    pub facts: Vec<MemoryFact>,
    pub updated_at: String,
}

/// Three-tier namespace scoping for memory isolation.
///
/// When a project is provided:
/// - **project**: per-agent, per-project → `agent_{name}_project_{slug}`
/// - **core**: cross-project agent expertise → `agent_{name}_core`
/// - **shared**: visible to all agents in the project → `project_{slug}`
///
/// When no project (system agents):
/// - All memory scopes collapse to `agent_{name}_core`
#[derive(Debug, Clone)]
pub struct MemoryScope {
    pub project: String,
    pub core: String,
    pub shared: String,
}

impl MemoryScope {
    /// Build a scope from agent name and optional project slug.
    ///
    /// When `project_slug` is `None`, all memory scopes collapse to
    /// `agent_{name}_core`.
    pub fn new(agent_name: &str, project_slug: Option<&str>) -> Self {
        let name = sanitize_name(agent_name);
        let core = format!("agent_{name}_core");

        match project_slug {
            Some(slug) if !slug.is_empty() => {
                let slug = sanitize_name(slug);
                Self {
                    project: format!("agent_{name}_project_{slug}"),
                    core: core.clone(),
                    shared: format!("project_{slug}"),
                }
            }
            _ => Self {
                project: core.clone(),
                core,
                shared: "shared".to_string(),
            },
        }
    }

    /// Reconstruct a full memory scope from a resolved namespace string when possible.
    ///
    /// Supported forms:
    /// - `agent_<name>_project_<slug>`
    /// - `agent_<name>_core`
    pub fn from_namespace(ns: &str) -> Option<Self> {
        if let Some((agent_prefix, slug)) = ns.rsplit_once("_project_")
            && agent_prefix.starts_with("agent_")
            && !slug.is_empty()
        {
            return Some(Self {
                project: ns.to_string(),
                core: format!("{agent_prefix}_core"),
                shared: format!("project_{slug}"),
            });
        }

        if ns.starts_with("agent_") && ns.ends_with("_core") {
            return Some(Self {
                project: ns.to_string(),
                core: ns.to_string(),
                shared: "shared".to_string(),
            });
        }

        None
    }

    /// Resolve a memory scope name ("project", "core", "shared") to a namespace string.
    pub fn resolve(&self, scope: &str) -> &str {
        match scope {
            "core" => &self.core,
            "shared" => &self.shared,
            _ => &self.project,
        }
    }

    /// All three memory namespaces for exhaustive search.
    pub fn all(&self) -> [&str; 3] {
        [&self.project, &self.core, &self.shared]
    }
}

/// Sanitize a name for use as a filesystem-safe directory component.
fn sanitize_name(name: &str) -> String {
    name.to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::MemoryScope;

    #[test]
    fn reconstructs_project_scope_from_namespace() {
        let scope = MemoryScope::from_namespace("agent_researcher_project_docs")
            .expect("project namespace should parse");
        assert_eq!(scope.project, "agent_researcher_project_docs");
        assert_eq!(scope.core, "agent_researcher_core");
        assert_eq!(scope.shared, "project_docs");
    }

    #[test]
    fn reconstructs_core_scope_from_namespace() {
        let scope =
            MemoryScope::from_namespace("agent_ops_core").expect("core namespace should parse");
        assert_eq!(scope.project, "agent_ops_core");
        assert_eq!(scope.core, "agent_ops_core");
        assert_eq!(scope.shared, "shared");
    }

    #[test]
    fn rejects_unknown_namespace_shapes() {
        assert!(MemoryScope::from_namespace("project_docs").is_none());
        assert!(MemoryScope::from_namespace("agent__project_").is_none());
        assert!(MemoryScope::from_namespace("totally_invalid").is_none());
    }
}
