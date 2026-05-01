use std::collections::BTreeSet;

use super::types::*;

impl BuiltinKnowledgePack {
    pub fn list_tree(&self, prefix: Option<&str>) -> BuiltinDocTree {
        let mut entries: Vec<_> = self
            .manifest
            .docs
            .iter()
            .filter(|doc| {
                prefix
                    .map(|prefix| doc.virtual_path.starts_with(prefix))
                    .unwrap_or(true)
            })
            .map(|doc| BuiltinDocTreeEntry {
                path: doc.virtual_path.clone(),
                title: doc.title.clone(),
                kind: doc.kind,
                tags: doc.tags.clone(),
            })
            .collect();
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        BuiltinDocTree {
            root_uri: self.manifest.root_uri.clone(),
            entries,
        }
    }

    pub fn list_docs(&self, filter: BuiltinDocFilter) -> Vec<&BuiltinDocManifest> {
        self.manifest
            .docs
            .iter()
            .filter(|doc| self.matches_filter(doc, &filter))
            .collect()
    }

    pub fn read_doc(&self, id_or_path: &str) -> Option<BuiltinDocRead> {
        let manifest = self.read_manifest(id_or_path)?.clone();
        let doc = self.doc_for_manifest(&manifest)?;
        Some(BuiltinDocRead {
            manifest,
            content: doc.content.to_string(),
        })
    }

    pub fn read_manifest(&self, id_or_path: &str) -> Option<&BuiltinDocManifest> {
        self.manifest
            .docs
            .iter()
            .find(|doc| doc.id == id_or_path || doc.virtual_path == id_or_path)
    }

    pub fn neighbors(
        &self,
        id_or_path: &str,
        edge_type: Option<BuiltinDocEdgeType>,
    ) -> Vec<&BuiltinDocManifest> {
        let Some(source) = self.read_manifest(id_or_path) else {
            return Vec::new();
        };

        source
            .related
            .iter()
            .filter(|edge| {
                edge_type
                    .map(|expected| edge.edge_type == expected)
                    .unwrap_or(true)
            })
            .filter_map(|edge| self.read_manifest(&edge.target))
            .collect()
    }

    pub fn search_paths(&self, query: &str, filter: BuiltinDocFilter) -> Vec<BuiltinDocSearchHit> {
        self.search(query, filter, false)
    }

    pub fn search_docs(&self, query: &str, filter: BuiltinDocFilter) -> Vec<BuiltinDocSearchHit> {
        self.search(query, filter, true)
    }

    fn search(
        &self,
        query: &str,
        filter: BuiltinDocFilter,
        include_content: bool,
    ) -> Vec<BuiltinDocSearchHit> {
        let needle = normalize(query);
        let mut hits = Vec::new();

        for manifest in self.list_docs(filter) {
            let mut score = 0;
            let mut matched = BTreeSet::new();

            score += score_field(&needle, &manifest.id, 100, "id", &mut matched);
            score += score_field(
                &needle,
                &manifest.virtual_path,
                90,
                "virtual_path",
                &mut matched,
            );
            score += score_field(&needle, &manifest.title, 80, "title", &mut matched);
            score += score_field(&needle, &manifest.summary, 60, "summary", &mut matched);

            for alias in &manifest.aliases {
                score += score_field(&needle, alias, 75, "alias", &mut matched);
            }
            for tag in &manifest.tags {
                score += score_field(&needle, tag, 70, "tag", &mut matched);
            }
            for keyword in &manifest.keywords {
                score += score_field(&needle, keyword, 65, "keyword", &mut matched);
            }

            let content = self.doc_for_manifest(manifest).map(|doc| doc.content);
            if let Some(content) = content {
                score += score_field(&needle, content, 20, "content", &mut matched);
            }

            if score > 0 || needle.is_empty() {
                hits.push(BuiltinDocSearchHit {
                    id: manifest.id.clone(),
                    virtual_path: manifest.virtual_path.clone(),
                    title: manifest.title.clone(),
                    summary: manifest.summary.clone(),
                    kind: manifest.kind,
                    authority: manifest.authority,
                    tags: manifest.tags.clone(),
                    score,
                    matched: matched.into_iter().collect(),
                    content: include_content.then(|| content.unwrap_or_default().to_string()),
                });
            }
        }

        hits.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then_with(|| a.virtual_path.cmp(&b.virtual_path))
        });
        hits
    }

    fn matches_filter(&self, doc: &BuiltinDocManifest, filter: &BuiltinDocFilter) -> bool {
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
                edge.target == *target
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

    fn doc_for_manifest(
        &self,
        manifest: &BuiltinDocManifest,
    ) -> Option<&'static BuiltinKnowledgeDoc> {
        self.docs
            .iter()
            .find(|doc| doc.id == manifest.id || doc.virtual_path == manifest.virtual_path)
    }
}

fn score_field(
    needle: &str,
    haystack: &str,
    weight: usize,
    label: &str,
    matched: &mut BTreeSet<String>,
) -> usize {
    if needle.is_empty() {
        return 1;
    }
    let haystack = normalize(haystack);
    if haystack == needle {
        matched.insert(label.to_string());
        weight * 2
    } else if haystack.contains(needle) {
        matched.insert(label.to_string());
        weight
    } else {
        0
    }
}

fn normalize(value: &str) -> String {
    value.trim().to_lowercase()
}
