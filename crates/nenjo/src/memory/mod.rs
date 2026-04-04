//! Memory system for persistent agent knowledge.
//!
//! The [`Memory`] trait defines the interface for storing and retrieving
//! agent facts and resources. The default [`MarkdownMemory`] backend uses
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
pub use prompt::{build_memory_vars, build_resource_vars};
pub use types::{MemoryCategory, MemoryFact, MemoryScope, ResourceEntry};

use anyhow::Result;

/// Trait for persistent agent memory and resource backends.
///
/// Memory operations are namespace-scoped. Namespaces isolate memory by
/// agent, project, and scope (project/core/shared).
///
/// Resource operations use `state/{ns}/resources/` paths for shared access.
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

    // -- Resources (shared documents) --

    /// Save a resource file with provenance metadata.
    async fn save_resource(
        &self,
        ns: &str,
        filename: &str,
        description: &str,
        created_by: &str,
        content: &str,
    ) -> Result<()>;

    /// List all resources in a namespace.
    async fn list_resources(&self, ns: &str) -> Result<Vec<ResourceEntry>>;

    /// Read a resource file's content.
    async fn read_resource(&self, ns: &str, filename: &str) -> Result<Option<String>>;

    /// Delete a resource file. Returns true if it existed.
    async fn delete_resource(&self, ns: &str, filename: &str) -> Result<bool>;
}
