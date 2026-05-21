use std::path::PathBuf;

/// Result type returned by `nenjo-packages`.
pub type Result<T> = std::result::Result<T, PackageError>;

/// Structured package registry/schema error.
#[derive(Debug, thiserror::Error)]
pub enum PackageError {
    #[error("invalid schema '{schema}': {reason}")]
    InvalidSchema { schema: String, reason: String },

    #[error("invalid package path '{path}': {reason}")]
    InvalidPath { path: String, reason: String },

    #[error("invalid package name '{name}': {reason}")]
    InvalidPackageName { name: String, reason: String },

    #[error("invalid package version '{version}': {reason}")]
    InvalidPackageVersion { version: String, reason: String },

    #[error("invalid package resource name '{name}': {reason}")]
    InvalidResourceName { name: String, reason: String },

    #[error("invalid module import '{reference}' on '{surface}': {reason}")]
    InvalidModuleImport {
        surface: String,
        reference: String,
        reason: String,
    },

    #[error("invalid resource manifest: {reason}")]
    InvalidResourceManifest { reason: String },

    #[error("duplicate package item '{item}' in {path}")]
    Duplicate { path: String, item: String },

    #[error("dependency resolution failed: {reason}")]
    Dependency { reason: String },

    #[error("package registry failed: {reason}")]
    Registry { reason: String },

    #[error("package source fetch failed: {reason}")]
    Fetch { reason: String },

    #[error("failed to read {path}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to parse {format}: {reason}")]
    Parse {
        format: &'static str,
        reason: String,
    },

    #[error("{message}")]
    Context {
        message: String,
        #[source]
        source: Box<PackageError>,
    },

    #[error("{0}")]
    Message(String),
}

impl PackageError {
    pub fn invalid_schema(schema: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::InvalidSchema {
            schema: schema.into(),
            reason: reason.into(),
        }
    }

    pub fn invalid_path(path: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::InvalidPath {
            path: path.into(),
            reason: reason.into(),
        }
    }

    pub fn invalid_package_name(name: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::InvalidPackageName {
            name: name.into(),
            reason: reason.into(),
        }
    }

    pub fn invalid_package_version(version: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::InvalidPackageVersion {
            version: version.into(),
            reason: reason.into(),
        }
    }

    pub fn invalid_resource_name(name: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::InvalidResourceName {
            name: name.into(),
            reason: reason.into(),
        }
    }

    pub fn invalid_module_import(
        surface: impl Into<String>,
        reference: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self::InvalidModuleImport {
            surface: surface.into(),
            reference: reference.into(),
            reason: reason.into(),
        }
    }

    pub fn invalid_resource_manifest(reason: impl Into<String>) -> Self {
        Self::InvalidResourceManifest {
            reason: reason.into(),
        }
    }

    pub fn dependency(reason: impl Into<String>) -> Self {
        Self::Dependency {
            reason: reason.into(),
        }
    }

    pub fn registry(reason: impl Into<String>) -> Self {
        Self::Registry {
            reason: reason.into(),
        }
    }

    pub fn fetch(reason: impl Into<String>) -> Self {
        Self::Fetch {
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

impl From<anyhow::Error> for PackageError {
    fn from(error: anyhow::Error) -> Self {
        Self::Message(format!("{error:?}"))
    }
}

impl From<serde_json::Error> for PackageError {
    fn from(error: serde_json::Error) -> Self {
        Self::Parse {
            format: "JSON",
            reason: error.to_string(),
        }
    }
}

impl From<serde_yaml::Error> for PackageError {
    fn from(error: serde_yaml::Error) -> Self {
        Self::Parse {
            format: "YAML",
            reason: error.to_string(),
        }
    }
}

impl From<reqwest::Error> for PackageError {
    fn from(error: reqwest::Error) -> Self {
        Self::fetch(error.to_string())
    }
}
