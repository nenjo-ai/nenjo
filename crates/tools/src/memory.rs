use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use std::str::FromStr;

// ── Layer types (from layers.rs) ──────────────────────────────────

/// Which layer of the memory hierarchy a piece of data belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryLayer {
    /// Raw immutable conversation/event logs.
    Resource,
    /// Extracted atomic facts with embeddings.
    Item,
    /// Evolving category summaries.
    Summary,
}

/// Lifecycle status of a memory item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum MemoryItemStatus {
    /// Live, searchable fact.
    #[default]
    Active,
    /// Replaced by a newer, more accurate fact.
    Superseded,
    /// Preserved but excluded from search results.
    Archived,
    /// Extracted but not yet validated.
    Unverified,
}

impl MemoryItemStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Superseded => "superseded",
            Self::Archived => "archived",
            Self::Unverified => "unverified",
        }
    }
}

impl FromStr for MemoryItemStatus {
    type Err = Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "active" => Ok(Self::Active),
            "superseded" => Ok(Self::Superseded),
            "archived" => Ok(Self::Archived),
            "unverified" => Ok(Self::Unverified),
            _ => Ok(Self::Active),
        }
    }
}

/// Filters for the `search_items_filtered` method.
#[derive(Debug, Clone, Default)]
pub struct SearchFilters {
    pub category: Option<String>,
    pub min_confidence: Option<f32>,
    pub max_age_days: Option<u32>,
    pub status: Option<String>,
}

/// An edge between two memory items.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryRelation {
    pub id: String,
    pub source_id: String,
    pub target_id: String,
    pub relation: String, // "supersedes", "depends_on", "related_to", "contradicts"
    pub created_at: String,
}

/// Layer 1: Raw immutable conversation log / event record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryResource {
    pub id: String,
    pub namespace: String,
    pub resource_type: String,
    pub content: String,
    pub metadata: serde_json::Value,
    pub created_at: String,
}

/// Layer 2: Extracted atomic fact with optional embedding.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryItem {
    pub id: String,
    pub namespace: String,
    pub fact: String,
    pub category: String,
    pub confidence: f32,
    #[serde(default)]
    pub status: MemoryItemStatus,
    pub source_resource_id: Option<String>,
    pub access_count: u32,
    pub last_accessed_at: String,
    pub created_at: String,
    pub updated_at: String,
    /// Set during search — hybrid relevance score (FTS5 BM25 + vector cosine).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
}

/// Layer 3: Evolving category summary, rebuilt as items change.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemorySummary {
    pub id: String,
    pub namespace: String,
    pub category: String,
    pub summary_text: String,
    pub item_count: u32,
    pub version: u32,
    pub created_at: String,
    pub updated_at: String,
}

// ── AgentMemory trait (from traits.rs) ────────────────────────────

/// Namespace-aware, three-layer memory backend.
///
/// All operations are scoped by namespace (`ns` parameter) to isolate
/// per-role and per-project memory. The three layers are:
///
/// - **Resource** (Layer 1): Raw immutable conversation/event logs.
/// - **Item** (Layer 2): Extracted atomic facts with embeddings.
/// - **Summary** (Layer 3): Evolving category summaries.
#[async_trait]
pub trait AgentMemory: Send + Sync {
    /// Backend name (e.g. "sqlite", "none").
    fn name(&self) -> &str;

    // ── Resource layer (raw immutable logs) ─────────────────────

    /// Store a raw resource (conversation log, event record).
    /// Returns the generated resource ID.
    async fn store_resource(
        &self,
        ns: &str,
        resource_type: &str,
        content: &str,
        metadata: serde_json::Value,
    ) -> anyhow::Result<String>;

    /// List resources in a namespace, optionally filtered by type.
    async fn list_resources(
        &self,
        ns: &str,
        resource_type: Option<&str>,
        limit: usize,
    ) -> anyhow::Result<Vec<MemoryResource>>;

    // ── Item layer (extracted atomic facts) ─────────────────────

    /// Store an extracted fact. Returns the generated item ID.
    async fn store_item(
        &self,
        ns: &str,
        fact: &str,
        category: &str,
        confidence: f32,
        source_resource_id: Option<&str>,
    ) -> anyhow::Result<String>;

    /// Hybrid search items (FTS5 BM25 + vector cosine) scoped to namespace.
    async fn search_items(
        &self,
        ns: &str,
        query: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<MemoryItem>>;

    /// List all items in a category within a namespace.
    async fn list_items_by_category(
        &self,
        ns: &str,
        category: &str,
    ) -> anyhow::Result<Vec<MemoryItem>>;

    /// Bump access_count and last_accessed_at for an item.
    async fn touch_item(&self, id: &str) -> anyhow::Result<()>;

    /// Delete an item by ID. Returns true if found and deleted.
    async fn delete_item(&self, id: &str) -> anyhow::Result<bool>;

    /// Delete items older than `older_than_days` with access_count < `min_access`.
    /// Returns the number of items deleted.
    async fn delete_stale_items(
        &self,
        ns: &str,
        older_than_days: u32,
        min_access: u32,
    ) -> anyhow::Result<u64>;

    // ── Status lifecycle ─────────────────────────────────────────

    /// Update the status of a memory item (active, superseded, archived, unverified).
    async fn update_item_status(&self, _id: &str, _status: &str) -> anyhow::Result<bool> {
        Ok(false)
    }

    /// Update the confidence score of a memory item (used for dedup merge + decay).
    async fn update_item_confidence(&self, _id: &str, _confidence: f32) -> anyhow::Result<bool> {
        Ok(false)
    }

    // ── Dedup ────────────────────────────────────────────────────

    /// Find items similar to a given fact text using vector similarity.
    /// Returns items with cosine similarity above `threshold`.
    async fn find_similar_items(
        &self,
        _ns: &str,
        _fact: &str,
        _threshold: f32,
        _limit: usize,
    ) -> anyhow::Result<Vec<MemoryItem>> {
        Ok(vec![])
    }

    // ── Decay ────────────────────────────────────────────────────

    /// Decay confidence of items not accessed in `older_than_hours` by `factor`.
    /// Returns the number of items decayed.
    async fn decay_confidence(
        &self,
        _ns: &str,
        _older_than_hours: u32,
        _factor: f32,
    ) -> anyhow::Result<u64> {
        Ok(0)
    }

    // ── Filtered search ──────────────────────────────────────────

    /// Search items with additional filters (category, confidence, age, status).
    async fn search_items_filtered(
        &self,
        ns: &str,
        query: &str,
        limit: usize,
        _filters: &SearchFilters,
    ) -> anyhow::Result<Vec<MemoryItem>> {
        self.search_items(ns, query, limit).await
    }

    // ── Relations ────────────────────────────────────────────────

    /// Create a relation between two memory items. Returns the relation ID.
    async fn add_relation(
        &self,
        _source_id: &str,
        _target_id: &str,
        _relation: &str,
    ) -> anyhow::Result<String> {
        Ok(String::new())
    }

    /// Get all relations for a memory item (as source or target).
    async fn get_relations(&self, _item_id: &str) -> anyhow::Result<Vec<MemoryRelation>> {
        Ok(vec![])
    }

    /// Get items related to a given item, traversing through relations.
    async fn get_related_items(
        &self,
        _item_id: &str,
        _limit: usize,
    ) -> anyhow::Result<Vec<MemoryItem>> {
        Ok(vec![])
    }

    // ── Summary layer (evolving category summaries) ─────────────

    /// Get the current summary for a category in a namespace.
    async fn get_summary(&self, ns: &str, category: &str) -> anyhow::Result<Option<MemorySummary>>;

    /// Delete a category summary. Returns true if found and deleted.
    async fn delete_summary(&self, _ns: &str, _category: &str) -> anyhow::Result<bool> {
        Ok(false)
    }

    /// Insert or update a category summary.
    async fn upsert_summary(
        &self,
        ns: &str,
        category: &str,
        text: &str,
        item_count: u32,
    ) -> anyhow::Result<()>;

    /// List all summaries in a namespace.
    async fn list_summaries(&self, ns: &str) -> anyhow::Result<Vec<MemorySummary>>;

    // ── Maintenance ─────────────────────────────────────────────

    /// List all distinct namespaces that have data.
    async fn list_all_namespaces(&self) -> anyhow::Result<Vec<String>>;

    /// Count total items in a namespace.
    async fn count_items(&self, ns: &str) -> anyhow::Result<usize>;

    /// Count items grouped by category in a namespace.
    async fn count_items_by_category(&self, _ns: &str) -> anyhow::Result<Vec<(String, usize)>> {
        Ok(vec![])
    }

    /// Quick health check (e.g. `SELECT 1`).
    async fn health_check(&self) -> bool;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_memory_trait_is_object_safe() {
        fn assert_object_safe<T: AgentMemory + ?Sized>() {}
        assert_object_safe::<dyn AgentMemory>();
    }

    #[test]
    fn memory_item_status_roundtrip() {
        for status in [
            MemoryItemStatus::Active,
            MemoryItemStatus::Superseded,
            MemoryItemStatus::Archived,
            MemoryItemStatus::Unverified,
        ] {
            let json = serde_json::to_string(&status).unwrap();
            let parsed: MemoryItemStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, status);
        }
    }

    #[test]
    fn memory_item_status_from_str() {
        assert_eq!(
            MemoryItemStatus::from_str("active").unwrap(),
            MemoryItemStatus::Active
        );
        assert_eq!(
            MemoryItemStatus::from_str("superseded").unwrap(),
            MemoryItemStatus::Superseded
        );
        assert_eq!(
            MemoryItemStatus::from_str("unknown").unwrap(),
            MemoryItemStatus::Active
        );
    }

    #[test]
    fn memory_item_status_default() {
        assert_eq!(MemoryItemStatus::default(), MemoryItemStatus::Active);
    }
}
