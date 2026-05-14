//! Memory system for persistent agent knowledge.
//!
//! The [`Memory`] trait defines the interface for storing and retrieving
//! agent facts and artifacts. The default [`MarkdownMemory`] backend uses
//! plain markdown files. Custom backends can implement the trait directly.
//!
//! # Usage
//!
//! ```ignore
//! use nenjo::memory::MarkdownMemory;
//!
//! let provider = Provider::builder()
//!     .with_loader(client)
//!     .with_model_factory(factory)
//!     .with_memory(MarkdownMemory::new("./state/memory", "./state"))
//!     .build()
//!     .await?;
//! ```

pub mod markdown;
pub mod prompt;
pub mod tools;
pub mod types;

pub use markdown::MarkdownMemory;
pub use prompt::{build_artifact_vars, build_memory_vars};
pub use types::{ArtifactEntry, MemoryCategory, MemoryFact, MemoryScope};

use anyhow::Result;
use std::sync::Arc;

/// Trait for persistent agent memory and artifact backends.
///
/// Memory operations are namespace-scoped. Namespaces isolate memory by
/// agent, project, and scope (project/core/shared).
///
/// Artifact operations use `state/{ns}/artifacts/` paths for shared access.
///
/// The default implementation is [`MarkdownMemory`] (file-based).
#[async_trait::async_trait]
pub trait Memory: Send + Sync {
    // -- Facts (category-grouped knowledge) --

    /// Append a fact to a category. Creates the category if it doesn't exist.
    async fn append(&self, ns: &str, category: &str, fact: &str) -> Result<()>;

    /// List all categories in a namespace with their facts.
    async fn list_categories(&self, ns: &str) -> Result<Vec<MemoryCategory>>;

    /// Read a single category.
    async fn read_category(&self, ns: &str, category: &str) -> Result<Option<MemoryCategory>>;

    /// Delete a specific fact from a category by exact text match.
    /// Returns true if the fact was found and removed.
    async fn delete_fact(&self, ns: &str, category: &str, fact: &str) -> Result<bool>;

    // -- Artifacts (shared documents) --

    /// Save an artifact file with provenance metadata.
    async fn save_artifact(
        &self,
        ns: &str,
        filename: &str,
        description: &str,
        created_by: &str,
        content: &str,
    ) -> Result<()>;

    /// List all artifacts in a namespace.
    async fn list_artifacts(&self, ns: &str) -> Result<Vec<ArtifactEntry>>;

    /// Read an artifact file's content.
    async fn read_artifact(&self, ns: &str, filename: &str) -> Result<Option<String>>;

    /// Delete an artifact file. Returns true if it existed.
    async fn delete_artifact(&self, ns: &str, filename: &str) -> Result<bool>;
}

#[async_trait::async_trait]
impl<T> Memory for Arc<T>
where
    T: Memory + ?Sized,
{
    async fn append(&self, ns: &str, category: &str, fact: &str) -> Result<()> {
        self.as_ref().append(ns, category, fact).await
    }

    async fn list_categories(&self, ns: &str) -> Result<Vec<MemoryCategory>> {
        self.as_ref().list_categories(ns).await
    }

    async fn read_category(&self, ns: &str, category: &str) -> Result<Option<MemoryCategory>> {
        self.as_ref().read_category(ns, category).await
    }

    async fn delete_fact(&self, ns: &str, category: &str, fact: &str) -> Result<bool> {
        self.as_ref().delete_fact(ns, category, fact).await
    }

    async fn save_artifact(
        &self,
        ns: &str,
        filename: &str,
        description: &str,
        created_by: &str,
        content: &str,
    ) -> Result<()> {
        self.as_ref()
            .save_artifact(ns, filename, description, created_by, content)
            .await
    }

    async fn list_artifacts(&self, ns: &str) -> Result<Vec<ArtifactEntry>> {
        self.as_ref().list_artifacts(ns).await
    }

    async fn read_artifact(&self, ns: &str, filename: &str) -> Result<Option<String>> {
        self.as_ref().read_artifact(ns, filename).await
    }

    async fn delete_artifact(&self, ns: &str, filename: &str) -> Result<bool> {
        self.as_ref().delete_artifact(ns, filename).await
    }
}
