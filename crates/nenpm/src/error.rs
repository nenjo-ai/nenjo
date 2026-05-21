use std::path::PathBuf;

/// Result type returned by `nenjo-nenpm`.
pub type Result<T> = std::result::Result<T, NenpmError>;

/// Structured nenpm package-manager error.
#[derive(Debug, thiserror::Error)]
pub enum NenpmError {
    #[error("invalid dependency manifest: {reason}")]
    DependencyManifest { reason: String },

    #[error("invalid package spec '{spec}': {reason}")]
    PackageSpec { spec: String, reason: String },

    #[error("registry resolution failed: {reason}")]
    Registry { reason: String },

    #[error("install failed: {reason}")]
    Install { reason: String },

    #[error("lockfile integrity failed: {reason}")]
    LockfileIntegrity { reason: String },

    #[error("package source failed: {reason}")]
    Source { reason: String },

    #[error("validation failed: {reason}")]
    Validation { reason: String },

    #[error("failed to read {path}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to write {path}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("serialization failed: {reason}")]
    Serialization { reason: String },

    #[error(transparent)]
    Package(#[from] nenjo_packages::PackageError),

    #[error("{message}")]
    Context {
        message: String,
        #[source]
        source: Box<NenpmError>,
    },

    #[error("{0}")]
    Message(String),
}

impl NenpmError {
    pub fn dependency_manifest(reason: impl Into<String>) -> Self {
        Self::DependencyManifest {
            reason: reason.into(),
        }
    }

    pub fn package_spec(spec: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::PackageSpec {
            spec: spec.into(),
            reason: reason.into(),
        }
    }

    pub fn registry(reason: impl Into<String>) -> Self {
        Self::Registry {
            reason: reason.into(),
        }
    }

    pub fn install(reason: impl Into<String>) -> Self {
        Self::Install {
            reason: reason.into(),
        }
    }

    pub fn integrity(reason: impl Into<String>) -> Self {
        Self::LockfileIntegrity {
            reason: reason.into(),
        }
    }

    pub fn source(reason: impl Into<String>) -> Self {
        Self::Source {
            reason: reason.into(),
        }
    }

    pub fn validation(reason: impl Into<String>) -> Self {
        Self::Validation {
            reason: reason.into(),
        }
    }

    pub fn serialization(reason: impl Into<String>) -> Self {
        Self::Serialization {
            reason: reason.into(),
        }
    }

    pub fn context(self, message: impl Into<String>) -> Self {
        Self::Context {
            message: message.into(),
            source: Box::new(self),
        }
    }
}

impl From<anyhow::Error> for NenpmError {
    fn from(error: anyhow::Error) -> Self {
        Self::Message(format!("{error:?}"))
    }
}

impl From<serde_json::Error> for NenpmError {
    fn from(error: serde_json::Error) -> Self {
        Self::serialization(error.to_string())
    }
}

impl From<serde_yaml::Error> for NenpmError {
    fn from(error: serde_yaml::Error) -> Self {
        Self::serialization(error.to_string())
    }
}

impl From<std::io::Error> for NenpmError {
    fn from(error: std::io::Error) -> Self {
        Self::Message(error.to_string())
    }
}
