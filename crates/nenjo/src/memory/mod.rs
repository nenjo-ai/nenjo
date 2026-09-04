//! Memory system for persistent agent knowledge.
//!
//! The [`Memory`] trait defines the interface for storing and retrieving
//! agent facts. The default [`MarkdownMemory`] backend uses plain markdown
//! files. Organization artifacts are provided by the platform artifact tools.
//!
//! # Usage
//!
//! ```ignore
//! use nenjo::memory::MarkdownMemory;
//!
//! let provider = Provider::builder()
//!     .with_loader(client)
//!     .with_model_factory(factory)
//!     .with_memory(MarkdownMemory::new("./state/memory"))
//!     .build()
//!     .await?;
//! ```

pub mod markdown;
pub mod prompt;
pub mod tools;
pub mod types;

pub use markdown::MarkdownMemory;
pub use prompt::build_memory_context;
pub use types::{MemoryCategory, MemoryFact, MemoryScope};

use anyhow::Result;
use std::sync::Arc;

/// Trait for persistent agent memory backends.
///
/// Memory operations are namespace-scoped. Namespaces isolate memory by
/// agent, project, and scope (project/core/shared).
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
}
