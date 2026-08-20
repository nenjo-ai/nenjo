//! Native media capability resolution.

pub mod artifact_router;
pub mod input_preparer;
pub mod pdf;
pub mod provider_analyzer;
pub mod resolver;

pub use artifact_router::{
    ArtifactAnalysisRequest, ArtifactAnalysisResult, ArtifactAnalyzer, ArtifactAnalyzerResolver,
    ArtifactInputDisposition, ArtifactInputRoute, ArtifactInputRouteError,
    ArtifactTransportResolver, ArtifactTransportTarget, AssignedArtifactAnalyzer,
    DirectArtifactInput, MaterializedAnalysisInput, PrimaryArtifactModel, collect_artifact_inputs,
};

pub use input_preparer::WorkerArtifactInputPreparer;
pub use pdf::{
    PDF_DERIVATION_VERSION, PdfDerivationError, PdfDerivativeCache, PdfDocumentDerivatives,
    PdfPageNumber, PdfPageText, RenderedPdfPage, derive_pdf,
};
pub use provider_analyzer::ProviderArtifactAnalyzer;
pub use resolver::{
    AgentModelAssignments, AssignmentSource, MediaCapabilitySource, MediaProviderResolver,
    MediaResolutionError, ModelAssignmentResolveError, ModelAssignmentResolver, ModelRuntimeConfig,
    ResolvedMediaProvider, ResolvedModelEndpoint, ResourceRef, validate_agent_media,
};
