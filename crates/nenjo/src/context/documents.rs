//! Library knowledge context helpers.

/// Build a compact XML listing of library item metadata from a library manifest file.
///
/// Returns empty string if no manifest exists or no documents are present.
pub async fn build_document_listing(docs_base_dir: &std::path::Path, project_slug: &str) -> String {
    let project_dir = docs_base_dir.join(project_slug);
    let manifest_path = project_dir.join("manifest.json");
    let manifest: nenjo_knowledge::KnowledgePackManifestData =
        match tokio::fs::read_to_string(&manifest_path)
            .await
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
        {
            Some(m) => m,
            None => return String::new(),
        };

    if manifest.docs.is_empty() {
        return String::new();
    }

    let ctx = crate::context::ProjectDocumentsContext {
        path: project_slug.to_string(),
        documents: manifest
            .docs
            .iter()
            .map(|doc| crate::context::DocumentContext {
                name: knowledge_doc_filename(doc),
                title: Some(doc.title.clone()),
                path: knowledge_doc_parent_path(doc),
                kind: Some(doc.kind.as_str().to_string()),
                authority: Some(doc.authority.as_str().to_string()),
                size: String::new(),
                status: Some(doc.status.as_str().to_string()),
                tags: doc.tags.clone(),
                aliases: doc.aliases.clone(),
                keywords: doc.keywords.clone(),
                summary: Some(doc.summary.clone()),
            })
            .collect(),
    };

    nenjo_xml::to_xml_pretty(&ctx, 2)
}

fn knowledge_doc_relative_path(doc: &nenjo_knowledge::KnowledgeDocManifest) -> String {
    doc.virtual_path
        .strip_prefix("project://")
        .and_then(|rest| rest.split_once('/').map(|(_, path)| path.to_string()))
        .unwrap_or_else(|| {
            doc.source_path
                .strip_prefix("docs/")
                .unwrap_or(doc.source_path.as_str())
                .to_string()
        })
}

fn knowledge_doc_filename(doc: &nenjo_knowledge::KnowledgeDocManifest) -> String {
    knowledge_doc_relative_path(doc)
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or("document.md")
        .to_string()
}

fn knowledge_doc_parent_path(doc: &nenjo_knowledge::KnowledgeDocManifest) -> Option<String> {
    knowledge_doc_relative_path(doc)
        .rsplit_once('/')
        .map(|(path, _)| path.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn document_listing_reads_synced_library_manifest_cache() {
        let dir = tempfile::tempdir().unwrap();
        let project_dir = dir.path().join("demo");
        std::fs::create_dir_all(&project_dir).unwrap();
        std::fs::write(
            project_dir.join("manifest.json"),
            r#"{
              "pack_id": "project-demo",
              "pack_version": "1",
              "schema_version": 1,
              "root_uri": "project://11111111-1111-1111-1111-111111111111/",
              "docs": [
                {
                  "id": "demo.nenjo",
                  "virtual_path": "project://11111111-1111-1111-1111-111111111111/domain/nenjo.md",
                  "source_path": "docs/domain/nenjo.md",
                  "title": "Nenjo Domain",
                  "summary": "Domain guidance for Nenjo",
                  "description": null,
                  "kind": "domain",
                  "authority": "canonical",
                  "status": "stable",
                  "tags": ["domain:nenjo"],
                  "aliases": ["nenjo"],
                  "keywords": ["agents", "routines"],
                  "related": []
                }
              ]
            }"#,
        )
        .unwrap();

        let listing = build_document_listing(dir.path(), "demo").await;

        assert!(listing.contains("<project_documents"));
        assert!(listing.contains("path=\"domain\""));
        assert!(listing.contains("name=\"nenjo.md\""));
        assert!(listing.contains("Domain guidance for Nenjo"));
    }
}
