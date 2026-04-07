//! Context block rendering for prompt generation.
//!
//! Contains Nenjo-specific context types, the context block renderer,
//! and the `RenderContext` for template variable building.
//! The generic template engine lives in `nenjo-prompts`.

pub mod renderer;
pub mod types;
pub mod var_defs;
pub mod vars;

pub use renderer::{ContextRenderer, InMemoryTemplateSource, TemplateSource};
pub use types::*;
pub use var_defs::{TemplateVarDef, TemplateVarGroup, template_var_defs, template_var_groups};
pub use vars::RenderContextVars;
