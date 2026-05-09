use std::borrow::Cow;
use std::path::{Component, Path, PathBuf};

use nenjo::knowledge::{
    KnowledgeDocFilter, KnowledgeDocManifest, KnowledgeDocTree, KnowledgeDocTreeEntry,
    KnowledgePack, KnowledgePackManifest, KnowledgePackManifestData,
};

#[derive(Debug, Clone)]
pub(crate) struct ProjectKnowledgePack {
    project_dir: PathBuf,
    manifest: KnowledgePackManifestData,
}

impl ProjectKnowledgePack {
    pub(crate) const MANIFEST_FILENAME: &'static str = "knowledge_manifest.json";

    pub(crate) fn load(project_dir: impl Into<PathBuf>) -> Option<Self> {
        let project_dir = project_dir.into();
        let manifest_path = project_dir.join(Self::MANIFEST_FILENAME);
        let content = std::fs::read_to_string(manifest_path).ok()?;
        let manifest = serde_json::from_str(&content).ok()?;
        Some(Self {
            project_dir,
            manifest,
        })
    }
}

impl KnowledgePack for ProjectKnowledgePack {
    fn manifest(&self) -> &dyn KnowledgePackManifest {
        &self.manifest
    }

    fn doc_content(&self, manifest: &KnowledgeDocManifest) -> Option<Cow<'_, str>> {
        let path = safe_relative_path(&manifest.source_path)?;
        std::fs::read_to_string(self.project_dir.join(path))
            .ok()
            .map(Cow::Owned)
    }

    fn read_manifest(&self, path: &str) -> Option<&KnowledgeDocManifest> {
        let normalized = normalize_project_doc_lookup(path, &self.manifest.root_uri);
        self.manifest.docs.iter().find(|doc| {
            doc.id == path
                || doc.virtual_path == path
                || normalize_project_doc_lookup(&doc.virtual_path, &self.manifest.root_uri)
                    == normalized
                || doc
                    .source_path
                    .strip_prefix("docs/")
                    .is_some_and(|source_path| source_path == normalized)
                || doc
                    .source_path
                    .rsplit('/')
                    .next()
                    .is_some_and(|filename| filename == normalized)
        })
    }

    fn list_docs(&self, mut filter: KnowledgeDocFilter) -> Vec<&KnowledgeDocManifest> {
        filter.path_prefix = filter
            .path_prefix
            .as_deref()
            .map(|prefix| normalize_project_path_prefix(prefix, &self.manifest.root_uri));
        if let Some(related_to) = filter.related_to.as_deref()
            && let Some(target) = self.read_manifest(related_to)
        {
            filter.related_to = Some(target.virtual_path.clone());
        }
        self.manifest
            .docs
            .iter()
            .filter(|doc| matches_project_filter(self, doc, &filter))
            .collect()
    }

    fn list_tree(&self, prefix: Option<&str>) -> KnowledgeDocTree {
        let prefix =
            prefix.map(|prefix| normalize_project_path_prefix(prefix, &self.manifest.root_uri));
        let mut entries: Vec<_> = self
            .manifest
            .docs
            .iter()
            .filter(|doc| {
                prefix
                    .as_deref()
                    .map(|prefix| doc.virtual_path.starts_with(prefix))
                    .unwrap_or(true)
            })
            .map(|doc| KnowledgeDocTreeEntry {
                path: doc.virtual_path.clone(),
                title: doc.title.clone(),
                kind: doc.kind,
                tags: doc.tags.clone(),
            })
            .collect();
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        KnowledgeDocTree {
            root_uri: self.manifest.root_uri.clone(),
            entries,
        }
    }
}

fn matches_project_filter(
    pack: &ProjectKnowledgePack,
    doc: &KnowledgeDocManifest,
    filter: &KnowledgeDocFilter,
) -> bool {
    if let Some(kind) = filter.kind
        && doc.kind != kind
    {
        return false;
    }
    if let Some(authority) = filter.authority
        && doc.authority != authority
    {
        return false;
    }
    if let Some(status) = filter.status
        && doc.status != status
    {
        return false;
    }
    if let Some(prefix) = &filter.path_prefix
        && !doc.virtual_path.starts_with(prefix)
    {
        return false;
    }
    if !filter.tags.is_empty()
        && !filter
            .tags
            .iter()
            .all(|tag| doc.tags.iter().any(|doc_tag| doc_tag == tag))
    {
        return false;
    }
    if let Some(target) = &filter.related_to {
        let has_edge = doc.related.iter().any(|edge| {
            let edge_matches_target = edge.target == *target
                || pack
                    .read_manifest(&edge.target)
                    .map(|edge_target| {
                        edge_target.id == *target || edge_target.virtual_path == *target
                    })
                    .unwrap_or(false);
            edge_matches_target
                && filter
                    .edge_type
                    .as_ref()
                    .map(|expected| edge.edge_type == *expected)
                    .unwrap_or(true)
        });
        if !has_edge {
            return false;
        }
    }
    true
}

fn normalize_project_doc_lookup(value: &str, root_uri: &str) -> String {
    value
        .trim()
        .strip_prefix(root_uri)
        .unwrap_or(value.trim())
        .trim_matches('/')
        .to_string()
}

fn normalize_project_path_prefix(value: &str, root_uri: &str) -> String {
    let trimmed = value.trim().trim_matches('/');
    if trimmed.is_empty() {
        return root_uri.to_string();
    }
    if value.trim().starts_with(root_uri) || value.trim().contains("://") {
        return value.trim().to_string();
    }
    format!("{root_uri}{trimmed}")
}

fn safe_relative_path(path: &str) -> Option<PathBuf> {
    let path = Path::new(path);
    let mut clean = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => clean.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    (!clean.as_os_str().is_empty()).then_some(clean)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nenjo::knowledge::{
        KnowledgeDocAuthority, KnowledgeDocKind, KnowledgeDocStatus, KnowledgePackManifestData,
    };

    fn project_manifest() -> KnowledgePackManifestData {
        KnowledgePackManifestData {
            pack_id: "project-test".into(),
            pack_version: "1".into(),
            schema_version: 1,
            root_uri: "project://test/".into(),
            content_hash: String::new(),
            docs: vec![KnowledgeDocManifest {
                id: "doc-1".into(),
                virtual_path: "project://test/architecture.md".into(),
                source_path: "docs/architecture.md".into(),
                title: "Architecture".into(),
                summary: "System architecture".into(),
                description: None,
                kind: KnowledgeDocKind::Reference,
                authority: KnowledgeDocAuthority::Reference,
                status: KnowledgeDocStatus::Stable,
                tags: vec!["architecture".into()],
                aliases: vec!["architecture.md".into()],
                keywords: vec!["system".into()],
                related: Vec::new(),
                size_bytes: 0,
                updated_at: String::new(),
            }],
        }
    }

    #[test]
    fn project_pack_reads_manifest_metadata_and_lazy_content() {
        let dir = tempfile::tempdir().unwrap();
        let docs_dir = dir.path().join("docs");
        std::fs::create_dir_all(&docs_dir).unwrap();
        std::fs::write(docs_dir.join("architecture.md"), "# Architecture").unwrap();

        let manifest = project_manifest();
        std::fs::write(
            dir.path().join(ProjectKnowledgePack::MANIFEST_FILENAME),
            serde_json::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();

        let pack = ProjectKnowledgePack::load(dir.path()).unwrap();

        let hits = pack.search_paths("Architecture", Default::default());
        assert_eq!(hits.len(), 1);
        assert!(hits[0].content.is_none());

        let doc = pack.read_doc("project://test/architecture.md").unwrap();
        assert_eq!(doc.content, "# Architecture");
    }

    #[test]
    fn project_pack_accepts_project_document_authority_values() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(ProjectKnowledgePack::MANIFEST_FILENAME),
            r#"{
              "pack_id": "project-test",
              "pack_version": "1",
              "schema_version": 1,
              "root_uri": "project://test/",
              "content_hash": "",
              "docs": [
                {
                  "id": "doc-1",
                  "virtual_path": "project://test/draft.md",
                  "source_path": "docs/draft.md",
                  "title": "Draft",
                  "summary": "Draft document",
                  "description": null,
                  "kind": "guide",
                  "authority": "draft",
                  "status": "draft",
                  "tags": [],
                  "aliases": [],
                  "keywords": [],
                  "related": []
                }
              ]
            }"#,
        )
        .unwrap();

        let pack = ProjectKnowledgePack::load(dir.path()).unwrap();

        assert_eq!(
            pack.manifest().docs()[0].authority,
            KnowledgeDocAuthority::Draft
        );
    }

    #[test]
    fn project_pack_rejects_unsafe_source_paths() {
        let dir = tempfile::tempdir().unwrap();
        let mut manifest = project_manifest();
        manifest.docs[0].source_path = "../secret.md".into();
        let pack = ProjectKnowledgePack {
            project_dir: dir.path().into(),
            manifest,
        };

        assert!(pack.read_doc("doc-1").is_none());
    }
}
