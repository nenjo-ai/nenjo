//! Memory system for persistent agent knowledge.
//!
//! The [`Memory`] trait defines the interface for storing and retrieving
//! agent facts. The default [`MarkdownMemory`] backend uses plain markdown
//! files with YAML frontmatter. Custom backends (SQLite, Redis, etc.) can
//! implement the trait directly.
//!
//! # Usage
//!
//! ```ignore
//! use nenjo::memory::MarkdownMemory;
//!
//! let provider = Provider::builder()
//!     .with_loader(client)
//!     .with_model_factory(factory)
//!     .with_memory(MarkdownMemory::new("./memory"))
//!     .build()
//!     .await?;
//! ```

pub mod markdown;
pub mod prompt;
pub mod tools;
pub mod types;

pub use markdown::MarkdownMemory;
pub use prompt::build_memory_xml;
pub use types::{MemoryItem, MemoryScope, MemoryStatus, MemorySummary};

use anyhow::Result;

/// Trait for persistent agent memory backends.
///
/// All operations are namespace-scoped. Namespaces isolate memory by
/// agent, project, and scope (project/core/shared).
///
/// The default implementation is [`MarkdownMemory`] (file-based).
/// Implement this trait for custom backends (SQLite, Redis, vector DBs, etc.).
#[async_trait::async_trait]
pub trait Memory: Send + Sync {
    // -- Items (atomic facts) --

    /// Store a fact and return its ID.
    async fn store(&self, ns: &str, fact: &str, category: &str, confidence: f64) -> Result<String>;

    /// Search for items matching a query. Returns scored results.
    async fn search(&self, ns: &str, query: &str, limit: usize) -> Result<Vec<MemoryItem>>;

    /// Delete an item by ID. Returns true if it existed.
    async fn delete(&self, id: &str) -> Result<bool>;

    /// Delete items older than `days` with fewer than `min_access` accesses.
    async fn delete_stale(&self, ns: &str, older_than_days: u64, min_access: u64) -> Result<u64>;

    // -- Summaries (category rollups) --

    /// Get the summary for a category, if it exists.
    async fn get_summary(&self, ns: &str, category: &str) -> Result<Option<MemorySummary>>;

    /// Create or update a category summary.
    async fn upsert_summary(
        &self,
        ns: &str,
        category: &str,
        text: &str,
        item_count: u32,
    ) -> Result<()>;

    /// List all summaries in a namespace.
    async fn list_summaries(&self, ns: &str) -> Result<Vec<MemorySummary>>;
}
