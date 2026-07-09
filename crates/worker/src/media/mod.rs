//! Native media capability resolution.

pub mod resolver;

pub use resolver::{
    AgentModelAssignments, AssignmentSource, MediaCapabilitySource, MediaProviderResolver,
    MediaResolutionError, ModelAssignmentResolveError, ModelAssignmentResolver, ModelRuntimeConfig,
    ResolvedMediaProvider, ResolvedModelEndpoint, ResourceRef, validate_agent_media,
};
