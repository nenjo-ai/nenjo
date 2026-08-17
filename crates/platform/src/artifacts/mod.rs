//! Authenticated access to immutable artifact bytes on the trusted harness host.

mod cache;
mod error;
mod materializer;
mod repository;

pub use error::ArtifactMaterializationError;
pub use materializer::{ArtifactMaterializer, MaterializedArtifact, PlatformArtifactMaterializer};
pub use repository::{ArtifactMetadata, ArtifactRepository, PlatformArtifactRepository};
