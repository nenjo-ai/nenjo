//! File-based markdown memory backend.
//!
//! Stores items and summaries as markdown files with YAML frontmatter.
//! No external dependencies (no SQLite, no vector search).
//!
//! Search uses keyword overlap scoring — devs who need vector search
//! should implement a custom [`Memory`] backend.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use uuid::Uuid;

use super::Memory;
use super::types::{MemoryItem, MemoryStatus, MemorySummary};

/// File-based markdown memory backend.
///
/// Directory layout:
/// ```text
/// {root}/
/// ├── {namespace}/
/// │   ├── items/
/// │   │   └── {id}.md
/// │   └── summaries/
/// │       └── {category}.md
/// ```
pub struct MarkdownMemory {
    root: PathBuf,
}

impl MarkdownMemory {
    /// Create a new markdown memory rooted at the given directory.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn ns_dir(&self, ns: &str) -> PathBuf {
        self.root.join(ns.replace(':', "_"))
    }

    fn items_dir(&self, ns: &str) -> PathBuf {
        self.ns_dir(ns).join("items")
    }

    fn summaries_dir(&self, ns: &str) -> PathBuf {
        self.ns_dir(ns).join("summaries")
    }

    /// Read all active items in a namespace.
    fn read_items(&self, ns: &str) -> Result<Vec<MemoryItem>> {
        let dir = self.items_dir(ns);
        if !dir.is_dir() {
            return Ok(Vec::new());
        }
        let mut items = Vec::new();
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "md") {
                if let Ok(item) = parse_item(&path) {
                    if item.status == MemoryStatus::Active {
                        items.push(item);
                    }
                }
            }
        }
        Ok(items)
    }
}

#[async_trait::async_trait]
impl Memory for MarkdownMemory {
    async fn store(&self, ns: &str, fact: &str, category: &str, confidence: f64) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        let dir = self.items_dir(ns);
        std::fs::create_dir_all(&dir)?;

        let now = chrono::Utc::now().to_rfc3339();
        let content = format!(
            "---\n\
             id: {id}\n\
             category: {category}\n\
             confidence: {confidence}\n\
             status: active\n\
             access_count: 0\n\
             created_at: {now}\n\
             ---\n\
             {fact}\n"
        );

        let path = dir.join(format!("{id}.md"));
        std::fs::write(&path, content)?;
        Ok(id)
    }

    async fn search(&self, ns: &str, query: &str, limit: usize) -> Result<Vec<MemoryItem>> {
        let items = self.read_items(ns)?;
        if items.is_empty() || query.is_empty() {
            return Ok(items.into_iter().take(limit).collect());
        }

        let query_words: Vec<String> = query
            .to_lowercase()
            .split_whitespace()
            .map(String::from)
            .collect();

        let mut scored: Vec<(f64, MemoryItem)> = items
            .into_iter()
            .map(|item| {
                let text = format!("{} {}", item.fact, item.category).to_lowercase();
                let matches = query_words
                    .iter()
                    .filter(|w| text.contains(w.as_str()))
                    .count();
                let score = matches as f64 / query_words.len().max(1) as f64;
                (score, item)
            })
            .filter(|(score, _)| *score > 0.0)
            .collect();

        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit);

        // Bump access counts
        for (_, item) in &scored {
            let path = self.items_dir(ns).join(format!("{}.md", item.id));
            if path.exists() {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    let bumped = content.replacen(
                        &format!("access_count: {}", item.access_count),
                        &format!("access_count: {}", item.access_count + 1),
                        1,
                    );
                    let _ = std::fs::write(&path, bumped);
                }
            }
        }

        Ok(scored.into_iter().map(|(_, item)| item).collect())
    }

    async fn delete(&self, id: &str) -> Result<bool> {
        // Search all namespace dirs for this item
        if !self.root.is_dir() {
            return Ok(false);
        }
        for entry in std::fs::read_dir(&self.root)? {
            let entry = entry?;
            let items_dir = entry.path().join("items");
            let path = items_dir.join(format!("{id}.md"));
            if path.exists() {
                std::fs::remove_file(&path)?;
                return Ok(true);
            }
        }
        Ok(false)
    }

    async fn delete_stale(&self, ns: &str, older_than_days: u64, min_access: u64) -> Result<u64> {
        let items = self.read_items(ns)?;
        let cutoff = chrono::Utc::now() - chrono::Duration::days(older_than_days as i64);
        let mut count = 0u64;

        for item in items {
            if item.access_count < min_access {
                if let Ok(created) = chrono::DateTime::parse_from_rfc3339(&item.created_at) {
                    if created < cutoff {
                        let path = self.items_dir(ns).join(format!("{}.md", item.id));
                        if path.exists() {
                            std::fs::remove_file(&path)?;
                            count += 1;
                        }
                    }
                }
            }
        }
        Ok(count)
    }

    async fn get_summary(&self, ns: &str, category: &str) -> Result<Option<MemorySummary>> {
        let path = self.summaries_dir(ns).join(format!("{category}.md"));
        if !path.exists() {
            return Ok(None);
        }
        parse_summary(&path).map(Some)
    }

    async fn upsert_summary(
        &self,
        ns: &str,
        category: &str,
        text: &str,
        item_count: u32,
    ) -> Result<()> {
        let dir = self.summaries_dir(ns);
        std::fs::create_dir_all(&dir)?;

        let now = chrono::Utc::now().to_rfc3339();
        let content = format!(
            "---\n\
             category: {category}\n\
             item_count: {item_count}\n\
             updated_at: {now}\n\
             ---\n\
             {text}\n"
        );

        let path = dir.join(format!("{category}.md"));
        std::fs::write(&path, content)?;
        Ok(())
    }

    async fn list_summaries(&self, ns: &str) -> Result<Vec<MemorySummary>> {
        let dir = self.summaries_dir(ns);
        if !dir.is_dir() {
            return Ok(Vec::new());
        }

        let mut summaries = Vec::new();
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "md") {
                if let Ok(summary) = parse_summary(&path) {
                    summaries.push(summary);
                }
            }
        }
        summaries.sort_by(|a, b| a.category.cmp(&b.category));
        Ok(summaries)
    }
}

// ---------------------------------------------------------------------------
// Parsing helpers
// ---------------------------------------------------------------------------

fn parse_item(path: &Path) -> Result<MemoryItem> {
    let content = std::fs::read_to_string(path)?;
    let (frontmatter, body) = split_frontmatter(&content)?;

    Ok(MemoryItem {
        id: extract_field(&frontmatter, "id")?,
        category: extract_field(&frontmatter, "category")?,
        confidence: extract_field(&frontmatter, "confidence")?
            .parse()
            .unwrap_or(0.5),
        status: match extract_field(&frontmatter, "status")?.as_str() {
            "superseded" => MemoryStatus::Superseded,
            "archived" => MemoryStatus::Archived,
            _ => MemoryStatus::Active,
        },
        access_count: extract_field(&frontmatter, "access_count")
            .unwrap_or_default()
            .parse()
            .unwrap_or(0),
        created_at: extract_field(&frontmatter, "created_at").unwrap_or_default(),
        fact: body.trim().to_string(),
    })
}

fn parse_summary(path: &Path) -> Result<MemorySummary> {
    let content = std::fs::read_to_string(path)?;
    let (frontmatter, body) = split_frontmatter(&content)?;

    Ok(MemorySummary {
        category: extract_field(&frontmatter, "category")?,
        item_count: extract_field(&frontmatter, "item_count")
            .unwrap_or_default()
            .parse()
            .unwrap_or(0),
        text: body.trim().to_string(),
    })
}

fn split_frontmatter(content: &str) -> Result<(String, String)> {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        anyhow::bail!("missing YAML frontmatter");
    }

    let after_first = &trimmed[3..];
    let end = after_first
        .find("\n---")
        .context("unterminated frontmatter")?;

    let frontmatter = after_first[..end].to_string();
    let body = after_first[end + 4..].to_string();
    Ok((frontmatter, body))
}

fn extract_field(frontmatter: &str, key: &str) -> Result<String> {
    let prefix = format!("{key}:");
    for line in frontmatter.lines() {
        let line = line.trim();
        if let Some(value) = line.strip_prefix(&prefix) {
            return Ok(value.trim().to_string());
        }
    }
    anyhow::bail!("missing field: {key}")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_memory() -> (tempfile::TempDir, MarkdownMemory) {
        let dir = tempfile::tempdir().unwrap();
        let memory = MarkdownMemory::new(dir.path());
        (dir, memory)
    }

    #[tokio::test]
    async fn store_and_search() {
        let (_dir, mem) = temp_memory();
        let ns = "test:ns";

        let id = mem
            .store(ns, "Rust is fast", "languages", 0.9)
            .await
            .unwrap();
        assert!(!id.is_empty());

        mem.store(ns, "Python is flexible", "languages", 0.8)
            .await
            .unwrap();
        mem.store(ns, "Always write tests", "practices", 0.95)
            .await
            .unwrap();

        let results = mem.search(ns, "Rust fast", 10).await.unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0].fact, "Rust is fast");
    }

    #[tokio::test]
    async fn delete_item() {
        let (_dir, mem) = temp_memory();
        let ns = "test:ns";

        let id = mem.store(ns, "temporary fact", "temp", 0.5).await.unwrap();
        assert!(mem.delete(&id).await.unwrap());
        assert!(!mem.delete(&id).await.unwrap()); // already gone

        let results = mem.search(ns, "temporary", 10).await.unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn summaries_crud() {
        let (_dir, mem) = temp_memory();
        let ns = "test:ns";

        assert!(mem.get_summary(ns, "prefs").await.unwrap().is_none());

        mem.upsert_summary(ns, "prefs", "User likes Rust", 3)
            .await
            .unwrap();
        let summary = mem.get_summary(ns, "prefs").await.unwrap().unwrap();
        assert_eq!(summary.category, "prefs");
        assert_eq!(summary.text, "User likes Rust");
        assert_eq!(summary.item_count, 3);

        mem.upsert_summary(ns, "prefs", "User loves Rust", 5)
            .await
            .unwrap();
        let updated = mem.get_summary(ns, "prefs").await.unwrap().unwrap();
        assert_eq!(updated.text, "User loves Rust");
        assert_eq!(updated.item_count, 5);

        let all = mem.list_summaries(ns).await.unwrap();
        assert_eq!(all.len(), 1);
    }

    #[tokio::test]
    async fn search_empty_namespace() {
        let (_dir, mem) = temp_memory();
        let results = mem.search("nonexistent", "anything", 10).await.unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn namespace_isolation() {
        let (_dir, mem) = temp_memory();

        mem.store("ns1", "fact in ns1", "cat", 0.9).await.unwrap();
        mem.store("ns2", "fact in ns2", "cat", 0.9).await.unwrap();

        let ns1 = mem.search("ns1", "fact", 10).await.unwrap();
        let ns2 = mem.search("ns2", "fact", 10).await.unwrap();

        assert_eq!(ns1.len(), 1);
        assert_eq!(ns2.len(), 1);
        assert!(ns1[0].fact.contains("ns1"));
        assert!(ns2[0].fact.contains("ns2"));
    }
}
