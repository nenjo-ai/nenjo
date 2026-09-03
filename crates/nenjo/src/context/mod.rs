//! Context block rendering for prompt generation.
//!
//! Contains Nenjo-specific context types, the context block renderer,
//! and canonical runtime-owned session and turn context serialization.
//! The generic template engine lives in `nenjo-xml`.

pub mod renderer;
pub(crate) mod runtime;
pub mod types;
pub mod vars;

pub use renderer::ContextRenderer;
pub use types::*;
pub use vars::RenderContextVars;
