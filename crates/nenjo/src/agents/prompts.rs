use crate::context::types::RenderContextBlock;
pub use crate::manifest::PromptConfig;

use crate::manifest::{ContextBlockManifest, ProjectManifest};
use crate::types::{ActiveDomain, RenderContextVars};

#[derive(Clone)]
pub struct PromptContext {
    /// Agent name (e.g. "manager", "architect").
    pub agent_name: String,
    /// Agent description for template variable rendering.
    pub agent_description: String,
    /// All available projects (used to resolve project slugs for paths).
    pub current_project: ProjectManifest,
    /// Active domain session (if the user is in a domain like /prd).
    pub active_domain: Option<ActiveDomain>,
    /// Whether the active domain's developer prompt addon should be appended.
    pub append_active_domain_addon: bool,
    /// Routine/project-level context fields injected by the executor.
    pub render_ctx_extra: RenderContextVars,
}
pub fn render_context_block(b: &ContextBlockManifest) -> RenderContextBlock {
    RenderContextBlock {
        name: b.name.clone(),
        path: b.path.clone(),
        template: b.template.clone(),
    }
}
