//! File-based markdown memory backend.
//!
//! Stores memory categories as markdown files with YAML frontmatter.
//!
//! Directory layout:
//! ```text
//! {memory_root}/
//! └── {namespace}/
//!     ├── {category}.md
//!     └── ...
//! ```

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::Memory;
use super::types::{MemoryCategory, MemoryFact};

/// File-based markdown memory backend.
pub struct MarkdownMemory {
    /// Root for memory categories (e.g. `~/.nenjo/state/memory/`).
    root: PathBuf,
}

impl MarkdownMemory {
    /// Create a new markdown memory rooted at `memory_root`.
    pub fn new(memory_root: impl Into<PathBuf>) -> Self {
        Self {
            root: memory_root.into(),
        }
    }

    fn ns_dir(&self, ns: &str) -> PathBuf {
        self.root.join(ns)
    }

    fn category_path(&self, ns: &str, category: &str) -> PathBuf {
        self.ns_dir(ns).join(format!("{category}.md"))
    }
}

#[async_trait::async_trait]
impl Memory for MarkdownMemory {
    async fn append(&self, ns: &str, category: &str, fact: &str) -> Result<()> {
        let path = self.category_path(ns, category);
        let parent = path.parent().ok_or_else(|| {
            anyhow::anyhow!("Invalid category path with no parent: {}", path.display())
        })?;
        tokio::fs::create_dir_all(parent).await?;

        let now = chrono::Utc::now().to_rfc3339();

        if tokio::fs::try_exists(&path).await? {
            // Read existing, append fact, update timestamp
            let content = tokio::fs::read_to_string(&path).await?;
            let (_fm, body) = split_frontmatter(&content)?;
            let mut facts_text = body.trim().to_string();
            if !facts_text.is_empty() {
                facts_text.push('\n');
            }
            facts_text.push_str(fact);

            let new_content =
                format!("---\ncategory: {category}\nupdated_at: {now}\n---\n{facts_text}\n");
            tokio::fs::write(&path, new_content).await?;
        } else {
            let content = format!("---\ncategory: {category}\nupdated_at: {now}\n---\n{fact}\n");
            tokio::fs::write(&path, content).await?;
        }
        Ok(())
    }

    async fn list_categories(&self, ns: &str) -> Result<Vec<MemoryCategory>> {
        let dir = self.ns_dir(ns);
        if !tokio::fs::try_exists(&dir).await? {
            return Ok(Vec::new());
        }

        let mut categories = Vec::new();
        let mut entries = tokio::fs::read_dir(&dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if entry.file_type().await?.is_file()
                && path.extension().is_some_and(|e| e == "md")
                && let Ok(cat) = parse_category(&path).await
            {
                categories.push(cat);
            }
        }
        categories.sort_by(|a, b| a.category.cmp(&b.category));
        Ok(categories)
    }

    async fn read_category(&self, ns: &str, category: &str) -> Result<Option<MemoryCategory>> {
        let path = self.category_path(ns, category);
        if !tokio::fs::try_exists(&path).await? {
            return Ok(None);
        }
        parse_category(&path).await.map(Some)
    }

    async fn delete_fact(&self, ns: &str, category: &str, fact: &str) -> Result<bool> {
        let path = self.category_path(ns, category);
        if !tokio::fs::try_exists(&path).await? {
            return Ok(false);
        }

        let content = tokio::fs::read_to_string(&path).await?;
        let (_fm, body) = split_frontmatter(&content)?;

        let lines: Vec<&str> = body.lines().filter(|l| !l.is_empty()).collect();
        let new_lines: Vec<&str> = lines
            .iter()
            .filter(|l| l.trim() != fact.trim())
            .copied()
            .collect();

        if new_lines.len() == lines.len() {
            return Ok(false); // fact not found
        }

        if new_lines.is_empty() {
            // No facts left — remove the file
            tokio::fs::remove_file(&path).await?;
        } else {
            let now = chrono::Utc::now().to_rfc3339();
            let facts_text = new_lines.join("\n");
            let new_content =
                format!("---\ncategory: {category}\nupdated_at: {now}\n---\n{facts_text}\n");
            tokio::fs::write(&path, new_content).await?;
        }
        Ok(true)
    }
}

// ---------------------------------------------------------------------------
// Parsing helpers
// ---------------------------------------------------------------------------

async fn parse_category(path: &Path) -> Result<MemoryCategory> {
    let content = tokio::fs::read_to_string(path).await?;
    let (frontmatter, body) = split_frontmatter(&content)?;

    let category = extract_field(&frontmatter, "category")?;
    let updated_at = extract_field(&frontmatter, "updated_at").unwrap_or_default();

    let facts: Vec<MemoryFact> = body
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| MemoryFact {
            text: l.trim().to_string(),
            created_at: String::new(), // not tracked per-line in this format
        })
        .collect();

    Ok(MemoryCategory {
        category,
        facts,
        updated_at,
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
        let mem_dir = tempfile::tempdir().unwrap();
        let memory = MarkdownMemory::new(mem_dir.path());
        (mem_dir, memory)
    }

    #[tokio::test]
    async fn append_and_list() {
        let (_md, mem) = temp_memory();
        let ns = "agent_test_core";

        mem.append(ns, "preferences", "User prefers Rust")
            .await
            .unwrap();
        mem.append(ns, "preferences", "Always use snake_case")
            .await
            .unwrap();
        mem.append(ns, "decisions", "Using PostgreSQL")
            .await
            .unwrap();

        let categories = mem.list_categories(ns).await.unwrap();
        assert_eq!(categories.len(), 2);
        assert_eq!(categories[0].category, "decisions");
        assert_eq!(categories[1].category, "preferences");
        assert_eq!(categories[1].facts.len(), 2);
        assert_eq!(categories[1].facts[0].text, "User prefers Rust");
        assert_eq!(categories[1].facts[1].text, "Always use snake_case");
    }

    #[tokio::test]
    async fn read_category() {
        let (_md, mem) = temp_memory();
        let ns = "agent_test_core";

        assert!(mem.read_category(ns, "prefs").await.unwrap().is_none());

        mem.append(ns, "prefs", "Likes Rust").await.unwrap();
        let cat = mem.read_category(ns, "prefs").await.unwrap().unwrap();
        assert_eq!(cat.category, "prefs");
        assert_eq!(cat.facts.len(), 1);
    }

    #[tokio::test]
    async fn delete_fact() {
        let (_md, mem) = temp_memory();
        let ns = "agent_test_core";

        mem.append(ns, "prefs", "Likes Rust").await.unwrap();
        mem.append(ns, "prefs", "Likes Go").await.unwrap();

        assert!(mem.delete_fact(ns, "prefs", "Likes Rust").await.unwrap());
        assert!(!mem.delete_fact(ns, "prefs", "Likes Rust").await.unwrap()); // already gone

        let cat = mem.read_category(ns, "prefs").await.unwrap().unwrap();
        assert_eq!(cat.facts.len(), 1);
        assert_eq!(cat.facts[0].text, "Likes Go");
    }

    #[tokio::test]
    async fn delete_last_fact_removes_file() {
        let (_md, mem) = temp_memory();
        let ns = "agent_test_core";

        mem.append(ns, "temp", "only fact").await.unwrap();
        assert!(mem.delete_fact(ns, "temp", "only fact").await.unwrap());
        assert!(mem.read_category(ns, "temp").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn empty_namespace() {
        let (_md, mem) = temp_memory();
        let cats = mem.list_categories("nonexistent").await.unwrap();
        assert!(cats.is_empty());
    }

    // -- Scoping tests --

    #[tokio::test]
    async fn memory_scope_isolation_project_agent() {
        let (_md, mem) = temp_memory();
        let scope = super::super::types::MemoryScope::new("coder", Some("myapp"));

        // Each tier writes to a different namespace
        mem.append(&scope.project, "prefs", "project fact")
            .await
            .unwrap();
        mem.append(&scope.core, "prefs", "core fact").await.unwrap();
        mem.append(&scope.shared, "prefs", "shared fact")
            .await
            .unwrap();

        // Each tier is isolated
        let proj = mem.list_categories(&scope.project).await.unwrap();
        assert_eq!(proj[0].facts[0].text, "project fact");

        let core = mem.list_categories(&scope.core).await.unwrap();
        assert_eq!(core[0].facts[0].text, "core fact");

        let shared = mem.list_categories(&scope.shared).await.unwrap();
        assert_eq!(shared[0].facts[0].text, "shared fact");

        // Verify namespace strings
        assert_eq!(scope.project, "agent_coder_project_myapp");
        assert_eq!(scope.core, "agent_coder_core");
        assert_eq!(scope.shared, "project_myapp");
    }

    #[tokio::test]
    async fn memory_scope_system_agent_collapses() {
        let (_md, mem) = temp_memory();
        let scope = super::super::types::MemoryScope::new("nenji", None);

        // Project and core resolve to the same namespace
        assert_eq!(scope.project, "agent_nenji_core");
        assert_eq!(scope.core, "agent_nenji_core");
        // Shared gets its own namespace
        assert_eq!(scope.shared, "shared");

        // Writing to project and core goes to the same dir
        mem.append(&scope.project, "prefs", "from project scope")
            .await
            .unwrap();
        mem.append(&scope.core, "prefs", "from core scope")
            .await
            .unwrap();

        let cats = mem.list_categories(&scope.core).await.unwrap();
        assert_eq!(cats[0].facts.len(), 2, "project + core should share a dir");

        // Shared is separate
        mem.append(&scope.shared, "team", "shared fact")
            .await
            .unwrap();
        let shared = mem.list_categories(&scope.shared).await.unwrap();
        assert_eq!(shared[0].facts.len(), 1);
    }

    #[tokio::test]
    async fn memory_scope_shared_visible_across_agents() {
        let (_md, mem) = temp_memory();
        let scope_a = super::super::types::MemoryScope::new("coder", Some("myapp"));
        let scope_b = super::super::types::MemoryScope::new("reviewer", Some("myapp"));

        // Both agents share the same shared namespace for the same project
        assert_eq!(scope_a.shared, scope_b.shared);
        assert_eq!(scope_a.shared, "project_myapp");

        mem.append(&scope_a.shared, "conventions", "Use Rust")
            .await
            .unwrap();

        let cats = mem.list_categories(&scope_b.shared).await.unwrap();
        assert_eq!(cats.len(), 1);
        assert_eq!(cats[0].facts[0].text, "Use Rust");
    }
}
