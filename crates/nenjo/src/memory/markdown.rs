//! File-based markdown memory backend.
//!
//! Stores memory categories as markdown files with YAML frontmatter.
//! Artifacts are stored as plain files with a `manifest.json` index.
//!
//! Both memories and artifacts live under `~/.nenjo/state/` so that all
//! agent-generated state can be backed up from a single directory. The
//! state dir is resolved as an absolute path, so it remains accessible
//! regardless of the current working directory (including git worktrees).
//!
//! Directory layout:
//! ```text
//! {state}/
//! ├── memory/
//! │   └── {namespace}/
//! │       ├── {category}.md      # memory category file
//! │       └── ...
//! └── {ns}/artifacts/
//!     ├── manifest.json          # artifact index
//!     ├── {file}                 # artifact file
//!     └── ...
//! ```

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::Memory;
use super::types::{ArtifactEntry, MemoryCategory, MemoryFact};

/// File-based markdown memory backend.
pub struct MarkdownMemory {
    /// Root for memory categories (e.g. `~/.nenjo/state/memory/`).
    root: PathBuf,
    /// Root for artifacts (e.g. `~/.nenjo/state/`).
    artifact_root: PathBuf,
}

impl MarkdownMemory {
    /// Create a new markdown memory with memory and artifact roots.
    ///
    /// Memory categories are stored under `memory_root/`.
    /// Artifacts are stored under `artifact_root/{ns}/`.
    pub fn new(memory_root: impl Into<PathBuf>, artifact_root: impl Into<PathBuf>) -> Self {
        Self {
            root: memory_root.into(),
            artifact_root: artifact_root.into(),
        }
    }

    fn ns_dir(&self, ns: &str) -> PathBuf {
        self.root.join(ns)
    }

    fn artifact_dir(&self, ns: &str) -> PathBuf {
        self.artifact_root.join(ns)
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

    async fn save_artifact(
        &self,
        ns: &str,
        filename: &str,
        description: &str,
        created_by: &str,
        content: &str,
    ) -> Result<()> {
        let dir = self.artifact_dir(ns);
        tokio::fs::create_dir_all(&dir).await?;

        // Write the file
        let file_path = dir.join(filename);
        tokio::fs::write(&file_path, content).await?;

        // Update manifest
        let manifest_path = dir.join("manifest.json");
        let mut entries = read_manifest(&manifest_path).await;

        // Remove existing entry with same filename (update)
        entries.retain(|e| e.filename != filename);

        let size_bytes = content.len() as i64;
        entries.push(ArtifactEntry {
            filename: filename.to_string(),
            description: description.to_string(),
            created_by: created_by.to_string(),
            size_bytes,
        });

        let json = serde_json::to_string_pretty(&entries)?;
        tokio::fs::write(&manifest_path, json).await?;
        Ok(())
    }

    async fn list_artifacts(&self, ns: &str) -> Result<Vec<ArtifactEntry>> {
        let manifest_path = self.artifact_dir(ns).join("manifest.json");
        Ok(read_manifest(&manifest_path).await)
    }

    async fn read_artifact(&self, ns: &str, filename: &str) -> Result<Option<String>> {
        let path = self.artifact_dir(ns).join(filename);
        if !tokio::fs::try_exists(&path).await? {
            return Ok(None);
        }
        tokio::fs::read_to_string(&path)
            .await
            .map(Some)
            .map_err(Into::into)
    }

    async fn delete_artifact(&self, ns: &str, filename: &str) -> Result<bool> {
        let dir = self.artifact_dir(ns);
        let file_path = dir.join(filename);
        if !tokio::fs::try_exists(&file_path).await? {
            return Ok(false);
        }
        tokio::fs::remove_file(&file_path).await?;

        // Update manifest
        let manifest_path = dir.join("manifest.json");
        let mut entries = read_manifest(&manifest_path).await;
        let before = entries.len();
        entries.retain(|e| e.filename != filename);
        if entries.len() != before {
            let json = serde_json::to_string_pretty(&entries)?;
            tokio::fs::write(&manifest_path, json).await?;
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

async fn read_manifest(path: &Path) -> Vec<ArtifactEntry> {
    tokio::fs::read_to_string(path)
        .await
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
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

    fn temp_memory() -> (tempfile::TempDir, tempfile::TempDir, MarkdownMemory) {
        let mem_dir = tempfile::tempdir().unwrap();
        let res_dir = tempfile::tempdir().unwrap();
        let memory = MarkdownMemory::new(mem_dir.path(), res_dir.path());
        (mem_dir, res_dir, memory)
    }

    #[tokio::test]
    async fn append_and_list() {
        let (_md, _wd, mem) = temp_memory();
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
        let (_md, _wd, mem) = temp_memory();
        let ns = "agent_test_core";

        assert!(mem.read_category(ns, "prefs").await.unwrap().is_none());

        mem.append(ns, "prefs", "Likes Rust").await.unwrap();
        let cat = mem.read_category(ns, "prefs").await.unwrap().unwrap();
        assert_eq!(cat.category, "prefs");
        assert_eq!(cat.facts.len(), 1);
    }

    #[tokio::test]
    async fn delete_fact() {
        let (_md, _wd, mem) = temp_memory();
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
        let (_md, _wd, mem) = temp_memory();
        let ns = "agent_test_core";

        mem.append(ns, "temp", "only fact").await.unwrap();
        assert!(mem.delete_fact(ns, "temp", "only fact").await.unwrap());
        assert!(mem.read_category(ns, "temp").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn empty_namespace() {
        let (_md, _wd, mem) = temp_memory();
        let cats = mem.list_categories("nonexistent").await.unwrap();
        assert!(cats.is_empty());
    }

    #[tokio::test]
    async fn artifact_crud() {
        let (_md, _wd, mem) = temp_memory();
        let ns = "workspace/artifacts";

        // Save
        mem.save_artifact(
            ns,
            "design.md",
            "System design doc",
            "architect",
            "# Design\nHere it is.",
        )
        .await
        .unwrap();

        // List
        let entries = mem.list_artifacts(ns).await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].filename, "design.md");
        assert_eq!(entries[0].description, "System design doc");
        assert_eq!(entries[0].created_by, "architect");

        // Read
        let content = mem.read_artifact(ns, "design.md").await.unwrap().unwrap();
        assert!(content.contains("# Design"));

        // Update (overwrite)
        mem.save_artifact(
            ns,
            "design.md",
            "Updated design",
            "architect",
            "# Design v2",
        )
        .await
        .unwrap();
        let entries = mem.list_artifacts(ns).await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].description, "Updated design");

        // Delete
        assert!(mem.delete_artifact(ns, "design.md").await.unwrap());
        assert!(!mem.delete_artifact(ns, "design.md").await.unwrap());
        assert!(mem.list_artifacts(ns).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn artifact_not_found() {
        let (_md, _wd, mem) = temp_memory();
        assert!(
            mem.read_artifact("artifacts", "nope.md")
                .await
                .unwrap()
                .is_none()
        );
    }

    // -- Scoping tests --

    #[tokio::test]
    async fn memory_scope_isolation_project_agent() {
        let (_md, _wd, mem) = temp_memory();
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
        let (_md, _wd, mem) = temp_memory();
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
        let (_md, _wd, mem) = temp_memory();
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

    #[tokio::test]
    async fn artifact_scope_project_under_state() {
        let (_md, rd, mem) = temp_memory();
        let scope = super::super::types::MemoryScope::new("architect", Some("myapp"));

        assert_eq!(scope.artifacts_project, "myapp/artifacts");
        assert_eq!(scope.artifacts_global, "artifacts");

        // Project artifact goes under {state}/myapp/artifacts/
        mem.save_artifact(
            &scope.artifacts_project,
            "prd.md",
            "Product requirements",
            "architect",
            "# PRD",
        )
        .await
        .unwrap();

        // Global artifact goes under {state}/artifacts/
        mem.save_artifact(
            &scope.artifacts_global,
            "standards.md",
            "Coding standards",
            "system",
            "# Standards",
        )
        .await
        .unwrap();

        // Verify files are in the artifact dir, not memory dir
        assert!(rd.path().join("myapp/artifacts/prd.md").exists());
        assert!(rd.path().join("artifacts/standards.md").exists());

        // Another agent on the same project sees the same artifacts
        let scope_b = super::super::types::MemoryScope::new("coder", Some("myapp"));
        assert_eq!(scope_b.artifacts_project, scope.artifacts_project);

        let entries = mem
            .list_artifacts(&scope_b.artifacts_project)
            .await
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].filename, "prd.md");
    }

    #[tokio::test]
    async fn artifact_scope_system_agent_global_only() {
        let scope = super::super::types::MemoryScope::new("nenji", None);

        // Both project and global resolve to the same "artifacts" path
        assert_eq!(scope.artifacts_project, "artifacts");
        assert_eq!(scope.artifacts_global, "artifacts");
    }
}
