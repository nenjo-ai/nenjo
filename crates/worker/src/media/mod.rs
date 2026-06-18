//! Native media capability resolution.

pub mod resolver;

pub use resolver::{
    MediaCapabilitySource, MediaProviderResolver, MediaResolutionError, ResolvedMediaProvider,
    validate_agent_media,
};
